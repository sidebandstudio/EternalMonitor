import Combine
import XCTest
@testable import EternalMonitor

/// A saved PC whose address changed: a new lease from the router, or the
/// PC moved between its cable and Wi-Fi.
@MainActor
final class MovedPCTests: XCTestCase {
    private static let token = Data(17...32)
    private var hostsToForget: [String] = []

    override func setUp() async throws {
        let store = RecentConnectionStore.shared
        let saved = store.connections
        let defaults = UserDefaults.standard.object(forKey: "recentConnections")
        addTeardownBlock { @MainActor in
            store.connections = saved
            UserDefaults.standard.set(defaults, forKey: "recentConnections")
            for address in self.hostsToForget { try? PairingStore.shared.remove(address: address) }
        }
    }

    private func found(_ name: String, _ address: String?, port: UInt16 = 9876) -> DiscoveredHost {
        DiscoveredHost(name: name, address: address, port: port)
    }

    // MARK: - The choice

    func testAStaleSavedAddressMovesToTheFreshBonjourAddress() {
        XCTAssertTrue(MovedPC.canLookUp(host: "192.168.1.20", pcName: "Studio PC", typed: false))
        let fresh = [found("EternalMonitor (Office PC)", "192.168.1.40"), found("EternalMonitor (Studio PC)", "192.168.1.31")]
        let moved = MovedPC.newAddress(pcName: "Studio PC", host: "192.168.1.20", port: 9876, found: fresh)
        XCTAssertEqual(moved?.host, "192.168.1.31")
        XCTAssertEqual(moved?.port, 9876)
        // A new port is a new address too.
        let port = MovedPC.newAddress(pcName: "Studio PC", host: "192.168.1.31", port: 9876,
                                      found: [found("EternalMonitor (Studio PC)", "192.168.1.31", port: 9900)])
        XCTAssertEqual(port?.port, 9900)
        // Still at the saved address: it is just not answering.
        XCTAssertNil(MovedPC.newAddress(pcName: "Studio PC", host: "192.168.1.31", port: 9876, found: fresh))
        XCTAssertNil(MovedPC.newAddress(pcName: "Studio PC", host: "192.168.1.20", port: 9876, found: []))
    }

    func testAManualAddressIsNeverLookedUpOrReplaced() {
        XCTAssertFalse(MovedPC.canLookUp(host: "192.168.1.20", pcName: "Studio PC", typed: true))
        // No name, and host names that DNS keeps current, aren't looked up.
        XCTAssertFalse(MovedPC.canLookUp(host: "192.168.1.20", pcName: nil, typed: false))
        XCTAssertFalse(MovedPC.canLookUp(host: "192.168.1.20", pcName: "", typed: false))
        XCTAssertFalse(MovedPC.canLookUp(host: "studio-pc.local", pcName: "Studio PC", typed: false))
        XCTAssertFalse(MovedPC.canLookUp(host: "fe80::1", pcName: "Studio PC", typed: false))

        for home in ["192.168.1.20", "10.0.0.5", "172.20.1.2", "169.254.3.4"] {
            XCTAssertTrue(MovedPC.replacesSavedEntry(home), home)
        }
        for kept in ["100.101.102.103", "127.0.0.1", "8.8.8.8", "studio-pc.local", "fe80::1", "192.168.1"] {
            XCTAssertFalse(MovedPC.replacesSavedEntry(kept), kept)
        }

        let store = RecentConnectionStore.shared
        store.connections = []
        store.add(host: "192.168.1.20", port: 9876, isUSB: false, name: "Studio PC", typed: true)
        store.add(host: "192.168.1.20", port: 9876, isUSB: false)
        XCTAssertEqual(store.entry(host: "192.168.1.20", port: 9876)?.typed, true, "picked from Recent, still typed")
        XCTAssertEqual(store.entry(host: "192.168.1.20", port: 9876)?.name, "Studio PC")

        // A Tailscale address stays for use away from home; a home address gives way.
        store.add(host: "100.101.102.103", port: 9876, isUSB: false, name: "Studio PC")
        store.moved(from: ("100.101.102.103", 9876), to: ("192.168.1.31", 9876), name: "Studio PC")
        XCTAssertNotNil(store.entry(host: "100.101.102.103", port: 9876))
        store.add(host: "192.168.1.22", port: 9876, isUSB: false, name: "Studio PC")
        store.moved(from: ("192.168.1.22", 9876), to: ("192.168.1.32", 9876), name: "Studio PC")
        XCTAssertNil(store.entry(host: "192.168.1.22", port: 9876))
        XCTAssertEqual(store.connections.first?.host, "192.168.1.32")
        XCTAssertEqual(store.connections.first?.name, "Studio PC")
    }

    func testTwoPCsWithTheSameNameAreNeverConfused() {
        let ours = found("EternalMonitor (Studio PC)", "192.168.1.31")
        // Bonjour renamed the second service: either one may be the saved PC.
        let renamed = found("EternalMonitor (Studio PC) (2)", "192.168.1.40")
        XCTAssertNil(MovedPC.newAddress(pcName: "Studio PC", host: "192.168.1.20", port: 9876, found: [ours, renamed]))
        XCTAssertNil(MovedPC.newAddress(pcName: "Studio PC", host: "192.168.1.20", port: 9876, found: [renamed]))
        // A second claim still resolving counts as well.
        XCTAssertNil(MovedPC.newAddress(pcName: "Studio PC", host: "192.168.1.20", port: 9876,
                                        found: [ours, found("EternalMonitor (Studio PC) (2)", nil)]))
        // Other PCs, however alike their names, are no claim.
        let others = [found("EternalMonitor (Studio PC 2)", "192.168.1.41"), found("EternalMonitor (Studio)", "192.168.1.42")]
        XCTAssertNil(MovedPC.newAddress(pcName: "Studio PC", host: "192.168.1.20", port: 9876, found: others))
        XCTAssertEqual(MovedPC.newAddress(pcName: "Studio PC", host: "192.168.1.20", port: 9876, found: others + [ours])?.host,
                       "192.168.1.31")
    }

    // MARK: - Connecting

    /// A saved PC named "Test PC" at `old`'s address, paired when `token` is set.
    private func savePC(at old: FakeUDPHost, token: Data?) throws {
        RecentConnectionStore.shared.connections = [
            RecentConnection(host: "127.0.0.1", port: old.port, lastUsed: .now, isUSB: false, name: "Test PC"),
        ]
        let address = PairingStore.address(host: "127.0.0.1", port: old.port)
        hostsToForget.append(address)
        if let token { try PairingStore.shared.save(token: token, hostName: "Test PC", address: address) }
    }

    /// A manager whose Bonjour finds "Test PC" at `new`'s port, and the
    /// service names it was asked for.
    private func manager(finding new: FakeUDPHost, connectTimeout: Double) -> (ConnectionManager, () -> [String]) {
        let manager = ConnectionManager(timing: .init(connectTimeoutSeconds: connectTimeout, reconnectGraceSeconds: 0.1,
                                                      reconnectBackoffSeconds: { _ in 0.05 }))
        hostsToForget.append(PairingStore.address(host: "127.0.0.1", port: new.port))
        var asked: [String] = []
        manager.findPCs = { name in
            asked.append(name)
            return [DiscoveredHost(name: "EternalMonitor (Test PC)", address: "127.0.0.1", port: new.port)]
        }
        return (manager, { asked })
    }

    private func until(_ description: String, seconds: Double, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    private func logged(_ manager: ConnectionManager, _ prefix: String) -> String? {
        manager.diagnostics.first { $0.message.hasPrefix(prefix) }?.message
    }

    /// The saved address stays silent; Bonjour has the same PC at another
    /// port, and the iPad streams from there with its pairing, no tap needed.
    func testASilentSavedAddressReconnectsWhereBonjourFoundThePC() async throws {
        let old = FakeUDPHost(), new = FakeUDPHost()
        try savePC(at: old, token: Self.token)
        let (manager, asked) = manager(finding: new, connectTimeout: 60)
        defer { manager.cancel() }
        // Every line logged: the new attempt clears Connection details.
        var lines: [String] = []
        let log = manager.$diagnostics.sink { if let line = $0.last?.message { lines.append(line) } }
        defer { log.cancel() }
        manager.connect(host: "127.0.0.1", port: old.port)
        let accepted = await Task.detached { new.acceptSession(authToken: Self.token) }.value
        XCTAssertTrue(accepted, "the iPad never tried the new address")
        XCTAssertEqual(asked(), ["EternalMonitor (Test PC)"])
        XCTAssertEqual(new.lastAuthToken, Self.token, "the saved pairing went along")
        try await until("handshake", seconds: 60) { manager.hostInfo != nil }
        new.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming", seconds: 60) { manager.state == .connected }
        XCTAssertTrue(lines.contains("Disconnect state=connecting reason=pcMoved"), lines.joined(separator: "\n"))
        // The saved address counts as not answering, in the closed set the backend accepts.
        let summary = lines.first { $0.hasPrefix("session_summary ") } ?? ""
        XCTAssertTrue(summary.contains(" stage=hello_sent outcome=no_reply failure=no_reply fallback=none end=superseded "), summary)

        let saved = RecentConnectionStore.shared.connections.first
        XCTAssertEqual(saved?.port, new.port)
        XCTAssertEqual(saved?.name, "Test PC")
        XCTAssertNotNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: old.port),
                        "loopback is no home address, so its entry stays")
        XCTAssertEqual(try PairingStore.shared.token(address: PairingStore.address(host: "127.0.0.1", port: new.port)), Self.token)
    }

    func testATypedAddressIsNeverLookedUp() async throws {
        let old = FakeUDPHost(), new = FakeUDPHost()
        try savePC(at: old, token: nil)
        let (manager, asked) = manager(finding: new, connectTimeout: 4)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: old.port, typed: true)
        try await until("timed out", seconds: 15) { manager.state == .disconnected }
        XCTAssertEqual(asked(), [])
        XCTAssertEqual(manager.problem, .noReply(usb: false))
    }

    /// A PC with the saved name at the new address that doesn't give the
    /// saved pairing back is another PC: the iPad leaves it and keeps its pairing.
    func testAnotherPCWithTheSameNameIsLeftAndThePairingKept() async throws {
        for answer in ["pairing off", "asks for a code"] {
            let old = FakeUDPHost(), new = FakeUDPHost()
            try savePC(at: old, token: Self.token)
            let (manager, _) = manager(finding: new, connectTimeout: 60)
            defer { manager.cancel() }
            manager.connect(host: "127.0.0.1", port: old.port)
            let accepted = await Task.detached {
                answer == "pairing off" ? new.acceptSession(authToken: Data(repeating: 0, count: 16))
                    : new.acceptSession(status: .unauthorized)
            }.value
            XCTAssertTrue(accepted, answer)
            try await until("left it: \(answer)", seconds: 30) { manager.state == .disconnected }
            XCTAssertEqual(manager.problem, .noReply(usb: false), answer)
            XCTAssertNil(manager.pairingPrompt, answer)
            XCTAssertNotNil(logged(manager, "The PC at the new address isn't the saved one"), answer)
            XCTAssertEqual(try PairingStore.shared.token(address: PairingStore.address(host: "127.0.0.1", port: old.port)),
                           Self.token, answer)
            XCTAssertNil(try PairingStore.shared.token(address: PairingStore.address(host: "127.0.0.1", port: new.port)), answer)
            XCTAssertNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: new.port), answer)
        }
    }

    /// Unpaired, the name is what tells PCs apart.
    func testAnUnpairedPCWithAnotherNameIsLeft() async throws {
        let old = FakeUDPHost(), new = FakeUDPHost()
        try savePC(at: old, token: nil)
        let (manager, _) = manager(finding: new, connectTimeout: 60)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: old.port)
        let accepted = await Task.detached { new.acceptSession(hostName: "Other PC", authToken: Data(repeating: 0, count: 16)) }.value
        XCTAssertTrue(accepted)
        try await until("left it", seconds: 30) { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .noReply(usb: false))
    }

    /// Found at a new address that doesn't answer either: the card says so.
    func testNoAnswerAtTheNewAddressSaysWhereItLooked() async throws {
        let old = FakeUDPHost(), new = FakeUDPHost()
        try savePC(at: old, token: nil)
        let (manager, asked) = manager(finding: new, connectTimeout: 5)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: old.port)
        let tried = await Task.detached { new.helloAttempts(within: 6) }.value
        XCTAssertGreaterThan(tried, 0)
        try await until("timed out", seconds: 15) { manager.state == .disconnected }
        XCTAssertEqual(asked().count, 1, "looked once, not again from the new address")
        XCTAssertEqual(manager.problem, .noReplyAtNewAddress("127.0.0.1"))
        XCTAssertEqual(manager.problem?.actions, [.retry])
    }
}
