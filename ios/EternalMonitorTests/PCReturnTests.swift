import Combine
import XCTest
@testable import EternalMonitor

/// Bonjour's list as a test hands it over. A search that starts lists
/// what Bonjour knows already, as a real one does in its first moments.
@MainActor
private final class FakePCBrowser: PCBrowser {
    var onHosts: (([DiscoveredHost]) -> Void)?
    /// What Bonjour has now.
    var current: [DiscoveredHost] = []
    private(set) var starts = 0
    private(set) var stops = 0
    var running: Bool { starts > stops }
    func start() {
        starts += 1
        onHosts?(current)
    }
    func stop() { stops += 1 }
    func list(_ hosts: [DiscoveredHost]) {
        current = hosts
        onHosts?(hosts)
    }
}

/// The iPad shows "didn't answer" because the PC app wasn't running yet.
/// When the PC's service then shows up on the network, it connects once by
/// itself, while the card is up, in the foreground, for a few minutes, and
/// only to the PC that proves it holds the saved pairing.
@MainActor
final class PCReturnTests: XCTestCase {
    nonisolated private static let token = Data(17...32)
    private let start = Date(timeIntervalSince1970: 1_000_000)

    private func pc(_ name: String = "EternalMonitor (Test PC)", _ address: String? = "127.0.0.1", port: UInt16,
                    version: String? = "0.3.5") -> DiscoveredHost {
        DiscoveredHost(name: name, address: address, port: port, version: version)
    }

    // MARK: - What counts as the PC showing up

    func testOnlyAListingThatWasNotThereBeforeCounts() {
        var watch = PCReturn(name: "Test PC", host: "192.0.2.20", port: 9876, at: start)
        let listed = pc(port: 9876)
        // What Bonjour already knew when the search began.
        watch.heard([listed], at: start.addingTimeInterval(0.2))
        watch.heard([listed], at: start.addingTimeInterval(PCReturn.settleSeconds + 1))
        XCTAssertNil(watch.appeared, "the PC was listed and didn't answer")
        // It leaves (the app closed) and comes back.
        watch.heard([], at: start.addingTimeInterval(5))
        XCTAssertNil(watch.appeared)
        watch.heard([listed], at: start.addingTimeInterval(9))
        XCTAssertEqual(watch.takeAppearance(), listed)
        XCTAssertNil(watch.takeAppearance(), "one try per appearance")
        // The same listing again is no new appearance; a change is, and the
        // try goes where the PC is now.
        watch.heard([listed], at: start.addingTimeInterval(10))
        XCTAssertNil(watch.appeared)
        watch.heard([pc(port: 9877)], at: start.addingTimeInterval(11))
        XCTAssertEqual(watch.takeAppearance()?.port, 9877, "a new port")
        watch.heard([pc(port: 9877, version: "0.3.6")], at: start.addingTimeInterval(12))
        XCTAssertEqual(watch.takeAppearance()?.version, "0.3.6", "a new version")
    }

    func testAPCMissingAtTheStartCountsAsSoonAsItShowsUp() {
        var watch = PCReturn(name: nil, host: "192.0.2.20", port: 9876, at: start)
        watch.heard([], at: start.addingTimeInterval(0.1))
        watch.heard([pc("EternalMonitor (Test PC)", "192.0.2.20", port: 9876)],
                    at: start.addingTimeInterval(PCReturn.settleSeconds + 0.5))
        XCTAssertNotNil(watch.appeared)
    }

    func testOnlyThisPCCounts() {
        var named = PCReturn(name: "Test PC", host: "192.0.2.20", port: 9876, at: start)
        var unnamed = PCReturn(name: nil, host: "192.0.2.20", port: 9876, at: start)
        let later = start.addingTimeInterval(5)
        let others = [pc("EternalMonitor (Other PC)", "192.0.2.30", port: 9876),
                      pc("EternalMonitor (Test PC 2)", "192.0.2.31", port: 9876),
                      pc("EternalMonitor (Other PC)", "192.0.2.20", port: 9900),
                      pc("EternalMonitor (Test PC)", nil, port: 9876)]
        named.heard(others, at: later)
        unnamed.heard(others, at: later)
        XCTAssertNil(named.appeared)
        XCTAssertNil(unnamed.appeared)
        // By name at another address, or by the address tried.
        named.heard(others + [pc("EternalMonitor (Test PC)", "192.0.2.40", port: 9876)], at: later)
        XCTAssertEqual(named.appeared?.address, "192.0.2.40")
        unnamed.heard(others + [pc("EternalMonitor (Any)", "192.0.2.20", port: 9876)], at: later)
        XCTAssertEqual(unnamed.appeared?.name, "EternalMonitor (Any)")
    }

    /// Back from the background, the new search lists what changed while
    /// the app was away as new, and what was there before as known.
    func testASearchStartedAgainKeepsWhatItKnew() {
        var watch = PCReturn(name: "Test PC", host: "192.0.2.20", port: 9876, at: start)
        watch.heard([pc(port: 9876)], at: start.addingTimeInterval(0.1))
        watch.restarted(at: start.addingTimeInterval(30))
        watch.heard([], at: start.addingTimeInterval(30.1))
        watch.heard([pc(port: 9876)], at: start.addingTimeInterval(30.2))
        XCTAssertNil(watch.appeared, "still the listing that didn't answer")

        var away = PCReturn(name: "Test PC", host: "192.0.2.20", port: 9876, at: start)
        away.heard([], at: start.addingTimeInterval(0.1))
        away.restarted(at: start.addingTimeInterval(30))
        away.heard([pc(port: 9876)], at: start.addingTimeInterval(30.2))
        XCTAssertNotNil(away.appeared, "it showed up while the app was away")
    }

    func testTheTimeLimitStartsWhenTheCardGoesUp() {
        var watch = PCReturn(name: nil, host: "192.0.2.20", port: 9876, at: start)
        XCTAssertFalse(watch.expired(at: start.addingTimeInterval(1_000), limit: 180), "not armed yet")
        watch.arm(at: start.addingTimeInterval(10))
        watch.arm(at: start.addingTimeInterval(100))
        XCTAssertFalse(watch.expired(at: start.addingTimeInterval(189), limit: 180))
        XCTAssertTrue(watch.expired(at: start.addingTimeInterval(190), limit: 180), "a second card keeps the first limit")
    }

    // MARK: - Connecting

    private final class Setup {
        let manager: ConnectionManager
        let browser: FakePCBrowser
        let host: FakeUDPHost
        let pairings: PairingStore
        var lines: [String] = []
        var log: AnyCancellable?
        var clock: Date

        init(manager: ConnectionManager, browser: FakePCBrowser, host: FakeUDPHost, pairings: PairingStore, clock: Date) {
            self.manager = manager
            self.browser = browser
            self.host = host
            self.pairings = pairings
            self.clock = clock
        }

        var tries: Int { lines.filter { $0 == "pc_return result=tried" }.count }
        var attempts: Int { lines.filter { $0.hasPrefix("Starting connection attempt=") }.count }
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

    /// A paired PC at loopback, saved under its name unless `named` is
    /// false, that doesn't answer. Loopback counts as a home address here.
    private func paired(named: Bool = true) throws -> Setup {
        let host = FakeUDPHost()
        let name = named ? "Test PC" : ""
        RecentConnectionStore.shared.connections = [
            RecentConnection(host: "127.0.0.1", port: host.port, lastUsed: .now, isUSB: false,
                             name: named ? name : nil, found: false),
        ]
        let pairings = PairingStore(storage: MemoryPairingStorage())
        try pairings.save(token: Self.token, hostName: name, address: PairingStore.address(host: "127.0.0.1", port: host.port))
        let manager = ConnectionManager(timing: .init(connectTimeoutSeconds: 4, reconnectGraceSeconds: 0.1,
                                                      reconnectBackoffSeconds: { _ in 0.05 }, pcReturnDebounceSeconds: 0.1),
                                        pairingStore: pairings)
        manager.homeAddress = { _ in true }
        manager.localRoute = { _, _ in MovedPC.Route(interface: "en0", direct: true, address: "127.0.0.1", netmask: "255.0.0.0") }
        manager.findPCs = { _ in [] }
        let browser = FakePCBrowser()
        manager.makePCBrowser = { browser }
        let setup = Setup(manager: manager, browser: browser, host: host, pairings: pairings, clock: start)
        manager.now = { [unowned setup] in setup.clock }
        setup.log = manager.$diagnostics.sink { [unowned setup] in
            if let line = $0.last?.message { setup.lines.append(line) }
        }
        return setup
    }

    /// The "didn't answer" card is up for `paired()`'s PC, and its watch armed.
    private func noReply(named: Bool = true) async throws -> Setup {
        let setup = try paired(named: named)
        setup.manager.connect(host: "127.0.0.1", port: setup.host.port)
        XCTAssertTrue(setup.browser.running, "the search runs from the attempt's start")
        // Bonjour has nothing for the PC: its app isn't open.
        setup.clock = start.addingTimeInterval(PCReturn.settleSeconds + 1)
        try await until("didn't answer", seconds: 15) { setup.manager.problem == .noReply(usb: false) }
        XCTAssertTrue(setup.manager.watchingForPC)
        XCTAssertTrue(setup.browser.running, "the search runs while the card is up")
        // Drop the unanswered HELLO2s, so only a new attempt's count.
        _ = await Task.detached { [host = setup.host] in host.helloAttempts(within: 0.4) }.value
        return setup
    }

    private func until(_ description: String, seconds: Double, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    private func settle(_ seconds: Double = 1) async throws {
        try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
    }

    /// The PC proves the pairing, then accepts and streams.
    private func stream(_ setup: Setup, from pc: FakeUDPHost) async throws {
        let accepted = await Task.detached { pc.answerIdentify(token: Self.token) && pc.acceptSession(authToken: Self.token) }.value
        XCTAssertTrue(accepted, "no automatic attempt reached the PC")
        XCTAssertFalse(pc.helloBeforeIdentify, "nothing with the pairing before the PC proved it holds it")
        try await until("handshake", seconds: 15) { setup.manager.hostInfo != nil }
        pc.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming", seconds: 30) { setup.manager.state == .connected }
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
        try await stream(setup, from: setup.host)
        XCTAssertEqual(setup.tries, 1)
        XCTAssertEqual(setup.attempts, 2)
        XCTAssertFalse(setup.manager.watchingForPC)
        XCTAssertFalse(setup.browser.running, "streaming ends the watch")
    }

    /// The PC opened just after the attempt's HELLO2s stopped, before the
    /// card: its listing is a return, not what Bonjour already knew.
    func testAPCOpenedJustAfterTheHellosIsNotMissed() async throws {
        let setup = try paired()
        defer { setup.manager.cancel() }
        setup.manager.connect(host: "127.0.0.1", port: setup.host.port)
        setup.clock = start.addingTimeInterval(PCReturn.settleSeconds + 0.5)
        setup.browser.list([setup.listing])
        try await until("didn't answer", seconds: 15) { setup.manager.problem == .noReply(usb: false) }
        _ = await Task.detached { [host = setup.host] in host.helloAttempts(within: 0.2) }.value
        try await stream(setup, from: setup.host)
        XCTAssertEqual(setup.tries, 1)
    }

    /// Bonjour has the saved PC at a new port: the try goes there, after the
    /// PC there proves the pairing, and the saved entry moves with it.
    func testAPCBackAtANewPortIsTriedThere() async throws {
        let setup = try await noReply()
        defer { setup.manager.cancel() }
        let moved = FakeUDPHost()
        setup.browser.list([DiscoveredHost(name: "EternalMonitor (Test PC)", address: "127.0.0.1", port: moved.port)])
        try await stream(setup, from: moved)
        let atOld = await Task.detached { [host = setup.host] in host.helloAttempts(within: 0.3) }.value
        XCTAssertEqual(atOld, 0, "nothing went to the old port")
        XCTAssertEqual(RecentConnectionStore.shared.connections.first?.port, moved.port)
        XCTAssertEqual(try setup.pairings.token(address: PairingStore.address(host: "127.0.0.1", port: moved.port)), Self.token)
    }

    /// Another PC took the saved address, with pairing off: it can't prove
    /// the pairing, so nothing is sent to it. One that got past that and
    /// accepts without giving the pairing back is dropped before a picture.
    func testADifferentPCAtTheSavedAddressGetsNoAutomaticSession() async throws {
        let setup = try await noReply(named: false)
        defer { setup.manager.cancel() }
        let other = DiscoveredHost(name: "EternalMonitor (Other PC)", address: "127.0.0.1", port: setup.host.port)
        setup.browser.list([other])
        let types = await Task.detached { [host = setup.host] in host.received(within: 4) }.value
        XCTAssertTrue(types.contains(.identify), "\(types)")
        XCTAssertFalse(types.contains(.hello2), "\(types)")
        try await until("not confirmed", seconds: 5) { setup.lines.contains("pc_return result=not_confirmed") }
        XCTAssertEqual(setup.attempts, 1)
        XCTAssertEqual(setup.manager.problem, .noReply(usb: false))

        setup.browser.list([])
        setup.browser.list([other])
        let accepted = await Task.detached { [host = setup.host] in
            host.answerIdentify(token: Self.token) && host.acceptSession(hostName: "Other PC", authToken: Data(repeating: 0, count: 16))
        }.value
        XCTAssertTrue(accepted)
        try await until("dropped", seconds: 15) { setup.lines.contains("pc_return result=other_pc") }
        XCTAssertEqual(setup.manager.state, .disconnected)
        XCTAssertNil(setup.manager.hostInfo)
        XCTAssertEqual(setup.manager.problem, .noReply(usb: false))
        XCTAssertEqual(try setup.pairings.token(address: PairingStore.address(host: "127.0.0.1", port: setup.host.port)), Self.token)
        XCTAssertFalse(setup.lines.contains { $0.hasPrefix("Connected to") })
    }

    /// The automatic try gets no answer either: the card comes back and
    /// waits for the PC to show up again. The same listing never brings
    /// another try.
    func testOneTryPerAppearance() async throws {
        let setup = try await noReply(named: false)
        defer { setup.manager.cancel() }
        let listing = DiscoveredHost(name: "EternalMonitor (Unknown)", address: "127.0.0.1", port: setup.host.port)
        setup.browser.list([listing])
        let proved = await Task.detached { [host = setup.host] in host.answerIdentify(token: Self.token) }.value
        XCTAssertTrue(proved)
        try await until("tried", seconds: 5) { setup.attempts == 2 }
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
        XCTAssertEqual(setup.tries, 0)
    }

    func testNoTryAfterTheTimeLimit() async throws {
        let setup = try await noReply()
        defer { setup.manager.cancel() }
        setup.clock = setup.clock.addingTimeInterval(ConnectionManager.Timing().pcReturnLimitSeconds)
        setup.browser.list([setup.listing])
        try await settle()
        XCTAssertEqual(setup.tries, 0)
        XCTAssertFalse(setup.manager.watchingForPC)
        XCTAssertFalse(setup.browser.running)
        XCTAssertTrue(setup.lines.contains("pc_return result=expired"))
    }

    /// Nothing starts in the background. Back in the foreground, a PC that
    /// showed up meanwhile gets its try.
    func testNoTryInTheBackground() async throws {
        let setup = try await noReply()
        defer { setup.manager.cancel() }
        setup.manager.handleAppBackgrounded()
        XCTAssertFalse(setup.browser.running, "no search in the background")
        setup.browser.current = [setup.listing]
        try await settle()
        XCTAssertEqual(setup.tries, 0)
        setup.manager.handleAppForegrounded()
        XCTAssertTrue(setup.browser.running)
        try await until("tried on return", seconds: 5) { setup.tries == 1 }
    }

    /// An unpaired PC can't prove it is the same one: it isn't watched for.
    func testAnUnpairedPCIsNotWatchedFor() async throws {
        let setup = try paired()
        defer { setup.manager.cancel() }
        try setup.pairings.forgetAll()
        setup.manager.connect(host: "127.0.0.1", port: setup.host.port)
        XCTAssertEqual(setup.browser.starts, 0)
        try await until("didn't answer", seconds: 15) { setup.manager.problem == .noReply(usb: false) }
        XCTAssertFalse(setup.manager.watchingForPC)
        XCTAssertEqual(setup.browser.starts, 0)
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
        let refused = await Task.detached { [host = setup.host] in
            host.answerIdentify(token: Self.token) && host.acceptSession(status: .busy)
        }.value
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
