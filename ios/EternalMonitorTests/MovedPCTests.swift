import Combine
import XCTest
@testable import EternalMonitor

/// Pairings kept in memory. `failSaves` makes every save fail, as a full
/// or locked Keychain does.
private final class MemoryPairingStorage: PairingStorage {
    var values: [String: Data] = [:]
    var failSaves = false
    func records() throws -> [PairingRecord] { values.map { PairingRecord(account: $0.key, token: $0.value) } }
    func save(account: String, token: Data) throws {
        if failSaves { throw KeychainPairingStorage.Failure(status: errSecNotAvailable) }
        values[account] = token
    }
    func remove(account: String?) throws {
        if let account { values.removeValue(forKey: account) } else { values.removeAll() }
    }
}

/// A saved PC whose address changed: a new lease from the router, or the
/// PC moved between its cable and Wi-Fi.
@MainActor
final class MovedPCTests: XCTestCase {
    nonisolated private static let token = Data(17...32)
    /// The choice tests' home network: documentation addresses stand in.
    private static let home: (String) -> Bool = { $0.hasPrefix("192.0.2.") || $0.hasPrefix("198.51.100.") }

    override func setUp() async throws {
        let store = RecentConnectionStore.shared
        let saved = store.connections
        let defaults = ["recentConnections", "lastHost", "lastPort"].map { ($0, UserDefaults.standard.object(forKey: $0)) }
        addTeardownBlock { @MainActor in
            store.connections = saved
            for (key, value) in defaults { UserDefaults.standard.set(value, forKey: key) }
        }
    }

    private func found(_ name: String, _ address: String?, port: UInt16 = 9876) -> DiscoveredHost {
        DiscoveredHost(name: name, address: address, port: port)
    }

    // MARK: - The choice

    func testAStaleSavedAddressMovesToTheFreshBonjourAddress() {
        let fresh = [found("EternalMonitor (Office PC)", "192.0.2.40"), found("EternalMonitor (Studio PC)", "192.0.2.31")]
        let moved = MovedPC.newAddress(pcName: "Studio PC", host: "192.0.2.20", port: 9876, found: fresh, homeAddress: Self.home)
        XCTAssertEqual(moved?.host, "192.0.2.31")
        XCTAssertEqual(moved?.port, 9876)
        // A new port is a new address too.
        let port = MovedPC.newAddress(pcName: "Studio PC", host: "192.0.2.31", port: 9876,
                                      found: [found("EternalMonitor (Studio PC)", "192.0.2.31", port: 9900)], homeAddress: Self.home)
        XCTAssertEqual(port?.port, 9900)
        // Still at the saved address: it is just not answering.
        XCTAssertNil(MovedPC.newAddress(pcName: "Studio PC", host: "192.0.2.31", port: 9876, found: fresh, homeAddress: Self.home))
        XCTAssertNil(MovedPC.newAddress(pcName: "Studio PC", host: "192.0.2.20", port: 9876, found: [], homeAddress: Self.home))
    }

    /// Only a paired PC that Bonjour listed, on a home network address,
    /// moves by itself. Typed, scanned and older entries, and VPN or
    /// Tailscale addresses on either side, never do.
    func testOnlyAPairedPCFoundWithBonjourOnAHomeAddressIsLookedUp() throws {
        XCTAssertTrue(MovedPC.canLookUp(host: "192.0.2.20", pcName: "Studio PC", found: true, paired: true, homeAddress: Self.home))
        XCTAssertFalse(MovedPC.canLookUp(host: "192.0.2.20", pcName: "Studio PC", found: false, paired: true, homeAddress: Self.home))
        XCTAssertFalse(MovedPC.canLookUp(host: "192.0.2.20", pcName: "Studio PC", found: true, paired: false, homeAddress: Self.home))
        XCTAssertFalse(MovedPC.canLookUp(host: "192.0.2.20", pcName: nil, found: true, paired: true, homeAddress: Self.home))
        XCTAssertFalse(MovedPC.canLookUp(host: "203.0.113.20", pcName: "Studio PC", found: true, paired: true, homeAddress: Self.home))

        for home in ["10.0.0.1", "10.255.255.254", "172.16.0.1", "172.31.255.254", "192.168.0.1", "192.168.255.254", "169.254.0.1"] {
            XCTAssertTrue(MovedPC.isHomeAddress(home), home)
        }
        for other in ["100.64.0.1", "100.127.255.254", "172.15.255.254", "172.32.0.1", "127.0.0.1", "192.0.2.1",
                      "studio-pc.local", "fe80::1", "192.168.1"] {
            XCTAssertFalse(MovedPC.isHomeAddress(other), other)
            XCTAssertFalse(MovedPC.canLookUp(host: other, pcName: "Studio PC", found: true, paired: true), other)
        }
        // Bonjour giving a VPN address is no move either.
        XCTAssertNil(MovedPC.newAddress(pcName: "Studio PC", host: "10.255.255.254", port: 9876,
                                        found: [found("EternalMonitor (Studio PC)", "100.64.0.9")]))

        // An entry saved before this version doesn't say where it came from.
        let legacy = #"[{"host":"192.0.2.20","port":9876,"lastUsed":0,"isUSB":false}]"#
        let decoded = try JSONDecoder().decode([RecentConnection].self, from: Data(legacy.utf8))
        XCTAssertNil(decoded.first?.found)

        let store = RecentConnectionStore.shared
        store.connections = []
        store.add(host: "192.0.2.20", port: 9876, isUSB: false, name: "Studio PC", found: true)
        store.add(host: "192.0.2.20", port: 9876, isUSB: false)
        XCTAssertEqual(store.entry(host: "192.0.2.20", port: 9876)?.found, true, "picked again from Recent, still found")
        XCTAssertEqual(store.entry(host: "192.0.2.20", port: 9876)?.name, "Studio PC")
        store.add(host: "192.0.2.21", port: 9876, isUSB: false, name: "Studio PC")
        XCTAssertEqual(store.entry(host: "192.0.2.21", port: 9876)?.found, false)

        store.moved(from: ("192.0.2.20", 9876), to: ("192.0.2.31", 9876), name: "Studio PC", replacesOld: false)
        XCTAssertNotNil(store.entry(host: "192.0.2.20", port: 9876))
        store.moved(from: ("192.0.2.20", 9876), to: ("192.0.2.32", 9876), name: "Studio PC", replacesOld: true)
        XCTAssertNil(store.entry(host: "192.0.2.20", port: 9876))
        XCTAssertEqual(store.connections.first?.host, "192.0.2.32")
        XCTAssertEqual(store.connections.first?.found, true)
    }

    func testTwoPCsWithTheSameNameAreNeverConfused() {
        let ours = found("EternalMonitor (Studio PC)", "192.0.2.31")
        // Bonjour renamed the second service: either one may be the saved PC.
        let renamed = found("EternalMonitor (Studio PC) (2)", "192.0.2.40")
        let choose = { MovedPC.newAddress(pcName: "Studio PC", host: "192.0.2.20", port: 9876, found: $0, homeAddress: Self.home) }
        XCTAssertNil(choose([ours, renamed]))
        XCTAssertNil(choose([renamed]))
        XCTAssertNil(choose([ours, found("EternalMonitor (Studio PC) (2)", nil)]))
        // Other PCs, however alike their names, are no claim.
        let others = [found("EternalMonitor (Studio PC 2)", "192.0.2.41"), found("EternalMonitor (Studio)", "192.0.2.42")]
        XCTAssertNil(choose(others))
        XCTAssertEqual(choose(others + [ours])?.host, "192.0.2.31")
    }

    /// The real scanner: a second service still resolving when the lookup
    /// ends has no address yet, and still makes the choice ambiguous.
    func testAServiceStillResolvingCountsAsAClaim() async throws {
        let scanner = NetworkScanner()
        defer { scanner.stopScan() }
        let choose = { MovedPC.newAddress(pcName: "Studio PC", host: "192.0.2.20", port: 9876, found: scanner.claims,
                                          homeAddress: Self.home) }
        scanner.merge(found("EternalMonitor (Studio PC)", "192.0.2.31"))
        XCTAssertEqual(choose()?.host, "192.0.2.31")
        scanner.netServiceBrowser(NetServiceBrowser(), didFind: NetService(domain: "local.", type: "_eternaldisplay._udp.",
                                                                          name: "EternalMonitor (Studio PC) (2)"), moreComing: false)
        let deadline = Date().addingTimeInterval(5)
        while scanner.claims.count < 2, Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(scanner.hosts.map(\.name), ["EternalMonitor (Studio PC)"], "not listed: no address yet")
        XCTAssertEqual(scanner.claims.map(\.name), ["EternalMonitor (Studio PC)", "EternalMonitor (Studio PC) (2)"])
        XCTAssertNil(choose())
    }

    /// The same known answer as the host's pairing tests and the wire's
    /// golden `identity` vector. The proof signs the PC's port and addresses.
    func testTheIdentityProofIsTheSharedKnownAnswer() {
        let addresses = [Data([192, 0, 2, 10]), Data([198, 51, 100, 10])]
        let proof = { (token: Data, port: UInt16, addresses: [Data]) in
            MovedPC.proof(token: token, nonce: Data(0xA0...0xAF), port: port, addresses: addresses)
        }
        let known = proof(Data(1...16), 9876, addresses)
        XCTAssertEqual(known.map { String(format: "%02x", $0) }.joined(),
                       "18fb81d76bbb96e91101119775ad9766b0c332b2c49441e6f07ec33be1c6d94e")
        XCTAssertNotEqual(proof(Data(2...17), 9876, addresses), known)
        XCTAssertNotEqual(proof(Data(1...16), 9877, addresses), known)
        XCTAssertNotEqual(proof(Data(1...16), 9876, [addresses[0]]), known)
    }

    /// Only an address this iPad reaches directly, over Wi-Fi or Ethernet
    /// and in that network's subnet, may be a moved PC. A VPN in a home
    /// network range is not.
    func testOnlyAnAddressOnTheLocalNetworkIsLocal() {
        let wifi = MovedPC.Route(interface: "en0", direct: true, address: "192.168.1.23", netmask: "255.255.255.0")
        XCTAssertTrue(MovedPC.isLocal("192.168.1.40", over: wifi))
        XCTAssertFalse(MovedPC.isLocal("192.168.2.40", over: wifi), "another subnet")
        XCTAssertFalse(MovedPC.isLocal("10.0.0.40", over: wifi))
        XCTAssertFalse(MovedPC.isLocal("studio-pc.local", over: wifi))
        let wide = MovedPC.Route(interface: "en0", direct: true, address: "10.20.0.5", netmask: "255.255.0.0")
        XCTAssertTrue(MovedPC.isLocal("10.20.200.9", over: wide))

        // A VPN in a home network range: the path isn't Wi-Fi or Ethernet,
        // or the interface is a tunnel, or it is a point-to-point address.
        let vpn = MovedPC.Route(interface: "utun4", direct: false, address: "10.8.0.6", netmask: "255.255.255.0")
        XCTAssertFalse(MovedPC.isLocal("10.8.0.1", over: vpn))
        XCTAssertFalse(MovedPC.isLocal("10.8.0.1", over: MovedPC.Route(interface: "utun4", direct: true, address: "10.8.0.6",
                                                                       netmask: "255.255.255.0")))
        XCTAssertFalse(MovedPC.isLocal("192.168.1.40", over: MovedPC.Route(interface: "ipsec0", direct: true,
                                                                           address: "192.168.1.23", netmask: "255.255.255.0")))
        XCTAssertFalse(MovedPC.isLocal("192.168.1.23", over: MovedPC.Route(interface: "en0", direct: true,
                                                                           address: "192.168.1.23", netmask: "255.255.255.255")))
        XCTAssertFalse(MovedPC.isLocal("192.168.1.40", over: MovedPC.Route(interface: "en0", direct: false,
                                                                           address: "192.168.1.23", netmask: "255.255.255.0")))
    }

    // MARK: - Connecting

    private struct Setup {
        let manager: ConnectionManager
        let storage: MemoryPairingStorage
        let pairings: PairingStore
        let asked: () -> [String]
    }

    /// A saved PC named "Test PC" at `old`, Bonjour-found unless `found` says
    /// otherwise, paired with `token`; Bonjour has it at `new`. Loopback
    /// counts as a home address here.
    private func setUp(old: FakeUDPHost, new: FakeUDPHost, token: Data?, found: Bool? = true,
                       route: MovedPC.Route? = MovedPC.Route(interface: "en0", direct: true, address: "127.0.0.1",
                                                             netmask: "255.0.0.0"),
                       connectTimeout: Double) throws -> Setup {
        RecentConnectionStore.shared.connections = [
            RecentConnection(host: "127.0.0.1", port: old.port, lastUsed: .now, isUSB: false, name: "Test PC", found: found),
        ]
        let storage = MemoryPairingStorage()
        let pairings = PairingStore(storage: storage)
        if let token { try pairings.save(token: token, hostName: "Test PC", address: PairingStore.address(host: "127.0.0.1", port: old.port)) }
        let manager = ConnectionManager(timing: .init(connectTimeoutSeconds: connectTimeout, reconnectGraceSeconds: 0.1,
                                                      reconnectBackoffSeconds: { _ in 0.05 }), pairingStore: pairings)
        manager.homeAddress = { _ in true }
        manager.localRoute = { _, _ in route }
        var asked: [String] = []
        manager.findPCs = { name in
            asked.append(name)
            return [DiscoveredHost(name: "EternalMonitor (Test PC)", address: "127.0.0.1", port: new.port)]
        }
        return Setup(manager: manager, storage: storage, pairings: pairings, asked: { asked })
    }

    private func until(_ description: String, seconds: Double, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    private func address(_ host: FakeUDPHost) -> String { PairingStore.address(host: "127.0.0.1", port: host.port) }

    private func stream(_ setup: Setup, from new: FakeUDPHost) async throws {
        let accepted = await Task.detached { new.answerIdentify(token: Self.token) && new.acceptSession(authToken: Self.token) }.value
        XCTAssertTrue(accepted, "the iPad never checked and tried the new address")
        try await until("handshake", seconds: 60) { setup.manager.hostInfo != nil }
        new.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming", seconds: 60) { setup.manager.state == .connected }
    }

    /// The saved address stays silent. Bonjour has the same PC at another
    /// port; it shows it holds the pairing before the token goes anywhere,
    /// and the iPad streams from there with no tap.
    func testASilentSavedAddressReconnectsOnceThePCShowsItIsTheSame() async throws {
        let old = FakeUDPHost(), new = FakeUDPHost()
        let setup = try setUp(old: old, new: new, token: Self.token, connectTimeout: 60)
        defer { setup.manager.cancel() }
        // Every line logged: the new attempt clears Connection details.
        var lines: [String] = []
        let log = setup.manager.$diagnostics.sink { if let line = $0.last?.message { lines.append(line) } }
        defer { log.cancel() }
        setup.manager.connect(host: "127.0.0.1", port: old.port)
        try await stream(setup, from: new)
        XCTAssertEqual(setup.asked(), ["EternalMonitor (Test PC)"])
        XCTAssertFalse(new.helloBeforeIdentify, "nothing with the token before the PC showed it is the same")
        XCTAssertEqual(new.lastAuthToken, Self.token, "then the saved pairing went along")
        XCTAssertTrue(lines.contains("Disconnect state=connecting reason=pcMoved"), lines.joined(separator: "\n"))
        let summary = lines.first { $0.hasPrefix("session_summary ") } ?? ""
        XCTAssertTrue(summary.contains(" stage=hello_sent outcome=no_reply failure=no_reply fallback=none end=superseded "), summary)

        let saved = RecentConnectionStore.shared.connections.first
        XCTAssertEqual(saved?.port, new.port)
        XCTAssertEqual(saved?.name, "Test PC")
        XCTAssertEqual(saved?.found, true)
        XCTAssertNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: old.port))
        XCTAssertEqual(try setup.pairings.token(address: address(new)), Self.token)
        XCTAssertNil(try setup.pairings.token(address: address(old)))
        XCTAssertEqual(UserDefaults.standard.integer(forKey: "lastPort"), Int(new.port))
    }

    /// A PC that gives a wrong answer, or none (a PC before 0.3.5), gets
    /// nothing more: no HELLO2, no token. The attempt ends as it always did.
    func testAPCThatDoesNotShowItIsTheSameIsLeftAlone() async throws {
        for answer in ["wrong proof", "no answer"] {
            let old = FakeUDPHost(), new = FakeUDPHost()
            let setup = try setUp(old: old, new: new, token: Self.token, connectTimeout: 7)
            defer { setup.manager.cancel() }
            setup.manager.connect(host: "127.0.0.1", port: old.port)
            let types = await Task.detached { () -> [Wire.PacketType] in
                if answer == "wrong proof", !new.answerIdentify(token: Data(2...17)) { return [] }
                return new.received(within: 6)
            }.value
            XCTAssertFalse(types.contains(.hello2), answer)
            XCTAssertFalse(new.helloBeforeIdentify, answer)
            if answer == "no answer" { XCTAssertEqual(Set(types), [.identify], answer) }
            try await until("timed out: \(answer)", seconds: 15) { setup.manager.state == .disconnected }
            XCTAssertEqual(setup.asked().count, 1, answer)
            XCTAssertEqual(setup.manager.problem, .noReply(usb: false), answer)
            XCTAssertEqual(try setup.pairings.token(address: address(old)), Self.token, answer)
            XCTAssertNotNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: old.port), answer)
        }
    }

    /// The answer at the address Bonjour gave comes from the saved PC at
    /// another address, and says so: it isn't the saved PC at this address.
    /// No HELLO2, no token.
    func testAnAnswerFromAnotherAddressIsNotTheSamePC() async throws {
        let old = FakeUDPHost(), forwarder = FakeUDPHost(), pc = FakeUDPHost()
        let setup = try setUp(old: old, new: forwarder, token: Self.token, connectTimeout: 7)
        defer { setup.manager.cancel() }
        setup.manager.connect(host: "127.0.0.1", port: old.port)
        async let answered = Task.detached { pc.answerIdentify(token: Self.token) }.value
        let types = await Task.detached { () -> [Wire.PacketType] in
            guard forwarder.forwardIdentify(to: pc) else { return [] }
            return forwarder.received(within: 4)
        }.value
        let pcAnswered = await answered
        XCTAssertTrue(pcAnswered, "the request reached the saved PC")
        XCTAssertFalse(types.isEmpty, "the iPad asked the forwarder")
        XCTAssertFalse(types.contains(.hello2), "\(types)")
        XCTAssertFalse(forwarder.helloBeforeIdentify)
        try await until("timed out", seconds: 15) { setup.manager.state == .disconnected }
        XCTAssertEqual(setup.manager.problem, .noReply(usb: false))
        XCTAssertEqual(try setup.pairings.token(address: address(old)), Self.token)
        XCTAssertNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: forwarder.port))
    }

    /// The PC at the address and port asked proves it holds the pairing but
    /// signs other addresses, or none: it isn't the saved PC at this
    /// address. No HELLO2, no token, and the pairing stays.
    func testAnAnswerSigningOtherAddressesIsNotTheSamePC() async throws {
        for signed in [[Data([192, 0, 2, 7])], []] {
            let old = FakeUDPHost(), new = FakeUDPHost()
            let setup = try setUp(old: old, new: new, token: Self.token, connectTimeout: 7)
            defer { setup.manager.cancel() }
            setup.manager.connect(host: "127.0.0.1", port: old.port)
            let types = await Task.detached { () -> [Wire.PacketType] in
                guard new.answerIdentify(token: Self.token, addresses: signed) else { return [] }
                return new.received(within: 4)
            }.value
            XCTAssertFalse(types.isEmpty, "\(signed): the iPad kept asking")
            XCTAssertFalse(types.contains(.hello2), "\(signed): \(types)")
            XCTAssertFalse(new.helloBeforeIdentify, "\(signed)")
            try await until("timed out", seconds: 15) { setup.manager.state == .disconnected }
            XCTAssertEqual(setup.manager.problem, .noReply(usb: false), "\(signed)")
            XCTAssertEqual(try setup.pairings.token(address: address(old)), Self.token, "\(signed)")
            XCTAssertNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: new.port), "\(signed)")
        }
    }

    /// Bonjour has the PC at a home network address that this iPad reaches
    /// over a VPN, or by no ready path: it isn't asked anything.
    func testAPCReachedOverAVPNIsNeverAsked() async throws {
        let vpn = MovedPC.Route(interface: "utun4", direct: false, address: "127.0.0.2", netmask: "255.0.0.0")
        for route: MovedPC.Route? in [vpn, nil] {
            let old = FakeUDPHost(), new = FakeUDPHost()
            let setup = try setUp(old: old, new: new, token: Self.token, route: route, connectTimeout: 6)
            defer { setup.manager.cancel() }
            setup.manager.connect(host: "127.0.0.1", port: old.port)
            let types = await Task.detached { new.received(within: 5) }.value
            XCTAssertEqual(types, [], "\(String(describing: route))")
            try await until("timed out", seconds: 15) { setup.manager.state == .disconnected }
            XCTAssertEqual(setup.asked().count, 1)
            XCTAssertEqual(setup.manager.problem, .noReply(usb: false))
            XCTAssertTrue(setup.manager.diagnostics.contains { $0.message.contains("not on this iPad's local network") })
            XCTAssertEqual(try setup.pairings.token(address: address(old)), Self.token)
        }
    }

    /// Unpaired, nothing proves which PC is which: with the saved PC gone, a
    /// same-named PC with pairing off isn't even looked for.
    func testAnUnpairedSavedPCIsNeverMoved() async throws {
        let old = FakeUDPHost(), new = FakeUDPHost()
        let setup = try setUp(old: old, new: new, token: nil, connectTimeout: 5)
        defer { setup.manager.cancel() }
        setup.manager.connect(host: "127.0.0.1", port: old.port)
        let types = await Task.detached { new.received(within: 4) }.value
        XCTAssertEqual(types, [])
        try await until("timed out", seconds: 15) { setup.manager.state == .disconnected }
        XCTAssertEqual(setup.asked(), [])
        XCTAssertEqual(setup.manager.problem, .noReply(usb: false))
        XCTAssertNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: new.port))
    }

    /// Entries that didn't come from Bonjour's list: typed, scanned, or saved
    /// by an older version, which doesn't say.
    func testAnEntryNotFoundWithBonjourIsNeverLookedUp() async throws {
        for found: Bool? in [nil, false] {
            let old = FakeUDPHost(), new = FakeUDPHost()
            let setup = try setUp(old: old, new: new, token: Self.token, found: found, connectTimeout: 4)
            defer { setup.manager.cancel() }
            setup.manager.connect(host: "127.0.0.1", port: old.port)
            try await until("timed out", seconds: 15) { setup.manager.state == .disconnected }
            XCTAssertEqual(setup.asked(), [], "\(String(describing: found))")
            XCTAssertEqual(setup.manager.problem, .noReply(usb: false))
        }
    }

    /// The PC at the new address showed it holds the pairing, then wasn't
    /// the saved PC by name. Try again goes back to the saved address and
    /// checks again; it never goes straight to the other PC.
    func testTryAgainAfterARefusalGoesBackToTheSavedAddress() async throws {
        let old = FakeUDPHost(), new = FakeUDPHost()
        let setup = try setUp(old: old, new: new, token: Self.token, connectTimeout: 60)
        defer { setup.manager.cancel() }
        setup.manager.connect(host: "127.0.0.1", port: old.port)
        let accepted = await Task.detached {
            new.answerIdentify(token: Self.token) && new.acceptSession(hostName: "Other PC", authToken: Data(repeating: 0, count: 16))
        }.value
        XCTAssertTrue(accepted)
        try await until("left it", seconds: 30) { setup.manager.state == .disconnected }
        XCTAssertEqual(setup.manager.problem, .noReply(usb: false))

        setup.manager.retry()
        let (atOld, atNew) = await Task.detached {
            (old.helloAttempts(within: 1.5), new.received(within: 1))
        }.value
        XCTAssertGreaterThan(atOld, 0, "Try again goes to the saved address")
        XCTAssertFalse(atNew.contains(.hello2), "and not straight to the other PC")
        XCTAssertNil(setup.manager.pairingPrompt)
        XCTAssertEqual(try setup.pairings.token(address: address(old)), Self.token)
    }

    /// The new address's pairing can't be saved: the stream goes on, and the
    /// saved entry and pairing stay where they were.
    func testTheSavedEntryStaysWhenTheNewPairingCantBeSaved() async throws {
        let old = FakeUDPHost(), new = FakeUDPHost()
        let setup = try setUp(old: old, new: new, token: Self.token, connectTimeout: 60)
        defer { setup.manager.cancel() }
        UserDefaults.standard.set("127.0.0.1", forKey: "lastHost")
        UserDefaults.standard.set(Int(old.port), forKey: "lastPort")
        setup.storage.failSaves = true
        setup.manager.connect(host: "127.0.0.1", port: old.port)
        try await stream(setup, from: new)
        XCTAssertEqual(try setup.pairings.token(address: address(old)), Self.token)
        XCTAssertNil(try setup.pairings.token(address: address(new)))
        XCTAssertNotNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: old.port))
        XCTAssertNil(RecentConnectionStore.shared.entry(host: "127.0.0.1", port: new.port))
        XCTAssertEqual(UserDefaults.standard.integer(forKey: "lastPort"), Int(old.port))
        XCTAssertTrue(setup.manager.diagnostics.contains { $0.message.hasPrefix("The PC's new address wasn't saved") })
    }

    /// The PC showed it is the same at its new address, then didn't answer
    /// HELLO2 there either: the card says where the iPad found it.
    func testNoAnswerAtTheNewAddressSaysWhereItLooked() async throws {
        let old = FakeUDPHost(), new = FakeUDPHost()
        let setup = try setUp(old: old, new: new, token: Self.token, connectTimeout: 7)
        defer { setup.manager.cancel() }
        setup.manager.connect(host: "127.0.0.1", port: old.port)
        let answered = await Task.detached { new.answerIdentify(token: Self.token) }.value
        XCTAssertTrue(answered)
        let tried = await Task.detached { new.helloAttempts(within: 3) }.value
        XCTAssertGreaterThan(tried, 0)
        try await until("timed out", seconds: 15) { setup.manager.state == .disconnected }
        XCTAssertEqual(setup.asked().count, 1, "looked once, not again from the new address")
        XCTAssertEqual(setup.manager.problem, .noReplyAtNewAddress("127.0.0.1"))
        XCTAssertEqual(setup.manager.problem?.actions, [.retry])
    }
}
