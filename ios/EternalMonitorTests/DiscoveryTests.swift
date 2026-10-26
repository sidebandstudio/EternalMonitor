import Darwin
import XCTest
@testable import EternalMonitor

/// A PC is only offered once it has an address the host's IPv4 listener
/// answers on, and saved PCs stay visible next to what discovery finds.
@MainActor
final class DiscoveryTests: XCTestCase {
    private func ipv4(_ text: String) -> Data {
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        inet_pton(AF_INET, text, &addr.sin_addr)
        return withUnsafeBytes(of: &addr) { Data($0) }
    }

    private func ipv6(_ text: String) -> Data {
        var addr = sockaddr_in6()
        addr.sin6_len = UInt8(MemoryLayout<sockaddr_in6>.size)
        addr.sin6_family = sa_family_t(AF_INET6)
        inet_pton(AF_INET6, text, &addr.sin6_addr)
        return withUnsafeBytes(of: &addr) { Data($0) }
    }

    func testLinkLocalIPv6AloneIsNotOfferedAsAnAddress() {
        // The case seen on the reference PC: Bonjour answered with only an
        // unscoped fe80:: address first.
        let host = DiscoveredHost.resolved(name: "EternalMonitor (STUDIO)", port: 9876,
            addresses: [ipv6("fe80::b38d:6347:a022:8ec5")])
        XCTAssertNil(host.address)
        XCTAssertTrue(host.finding)
        XCTAssertEqual(ConnectView.discoveredSubtitle(host), "Finding…")
        XCTAssertFalse(ConnectView.discoveredSubtitle(host).contains("fe80"))

        let global = DiscoveredHost.resolved(name: "PC", port: 9876, addresses: [ipv6("2001:db8::20")])
        XCTAssertNil(global.address, "the host listens on IPv4 only")
    }

    func testIPv4IsPreferredWhateverOrderBonjourAnswers() {
        let host = DiscoveredHost.resolved(name: "PC", port: 9876,
            addresses: [ipv6("fe80::1"), ipv4("192.168.1.20"), ipv6("2001:db8::20")])
        XCTAssertEqual(host.address, "192.168.1.20")
        XCTAssertFalse(host.finding)
        XCTAssertEqual(ConnectView.discoveredSubtitle(host), "Tap to connect · 192.168.1.20")
        XCTAssertEqual(DiscoveredHost.resolved(name: "PC", port: 0, addresses: [ipv4("10.0.0.5")]).port, 9876)
    }

    func testAPCStillBeingFoundNeverReplacesOneWithAnAddress() {
        let scanner = NetworkScanner()
        scanner.merge(DiscoveredHost.resolved(name: "PC", port: 9876, addresses: [ipv6("fe80::1")]))
        XCTAssertEqual(scanner.hosts.map(\.address), [nil])
        scanner.merge(DiscoveredHost(name: "PC", address: "192.168.1.20", port: 9876))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.20"])
        // A later IPv6-only answer for the same PC keeps the IPv4 entry.
        scanner.merge(DiscoveredHost.resolved(name: "PC", port: 9876, addresses: [ipv6("fe80::1")]))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.20"])
        let gaveUp = DiscoveredHost(name: "Other", address: nil, port: 9876, finding: false)
        XCTAssertTrue(ConnectView.discoveredSubtitle(gaveUp).contains("Type the Address"))
    }

    /// A slow IPv4 lookup can't bring back a PC that left the network, or
    /// replace the address a newer scan found.
    func testALateIPv4LookupCannotBringBackOrOverwriteAPC() async throws {
        let lookup = HeldLookup()
        let scanner = NetworkScanner()
        scanner.resolveIPv4 = { lookup.resolve($0) }
        // A lookup still wanted gives its PC the address.
        let other = DiscoveredHost.resolved(name: "Other", port: 9876, addresses: [ipv6("fe80::2")])
        scanner.merge(other)
        scanner.lookUpIPv4(hostName: "other.local.", service: other)
        lookup.answer("192.168.1.30")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.30"])

        // The PC leaves the network while its lookup runs.
        let found = DiscoveredHost.resolved(name: "PC", port: 9876, addresses: [ipv6("fe80::1")])
        scanner.merge(found)
        scanner.lookUpIPv4(hostName: "studio.local.", service: found)
        scanner.netServiceBrowser(NetServiceBrowser(),
            didRemove: NetService(domain: "local.", type: "_eternaldisplay._udp.", name: "PC"), moreComing: false)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(scanner.hosts.map(\.name), ["Other"])
        lookup.answer("192.168.1.20")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(scanner.hosts.map(\.name), ["Other"], "a PC that left must not come back")

        // A new scan finds the PC at a newer address before an older lookup answers.
        scanner.merge(found)
        scanner.lookUpIPv4(hostName: "studio.local.", service: found)
        scanner.startScan()
        scanner.merge(DiscoveredHost(name: "PC", address: "192.168.1.21", port: 9876))
        // Stop the real Bonjour browse before the run loop can deliver
        // anything from it: a PC running EternalMonitor on this network
        // would otherwise join the list. The old lookup still belongs to
        // the previous scan.
        scanner.stopScan()
        lookup.answer("192.168.1.20")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.21"])
    }

    /// Bonjour can answer a resolve after its PC left or a new scan began.
    /// That answer can't list the PC again or start a lookup for it.
    func testALateResolveCannotBringBackAPC() async throws {
        let lookups = CountedLookup()
        let scanner = NetworkScanner()
        scanner.resolveIPv4 = { lookups.resolve($0) }
        let service = HeldService(name: "PC", hostName: "studio.local.")
        scanner.netServiceBrowser(NetServiceBrowser(), didFind: service, moreComing: false)
        try await Task.sleep(for: .milliseconds(100))

        // The browser reports the removal with its own NetService object.
        scanner.netServiceBrowser(NetServiceBrowser(),
            didRemove: NetService(domain: "local.", type: "_eternaldisplay._udp.", name: "PC"), moreComing: false)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(service.stopped, "removing a PC stops its resolve")
        XCTAssertNil(service.delegate)
        service.addresses = [ipv6("fe80::1")]
        scanner.netServiceDidResolveAddress(service)
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(scanner.hosts.map(\.name), [], "a PC that left must not come back")
        XCTAssertEqual(lookups.count, 0)

        // A resolve started by the previous scan doesn't count in a new one.
        let earlier = HeldService(name: "PC", hostName: "studio.local.")
        scanner.netServiceBrowser(NetServiceBrowser(), didFind: earlier, moreComing: false)
        try await Task.sleep(for: .milliseconds(100))
        scanner.startScan()
        defer { scanner.stopScan() }
        earlier.addresses = [ipv4("192.168.1.20")]
        scanner.netServiceDidResolveAddress(earlier)
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertFalse(scanner.hosts.contains { $0.name == "PC" })
    }

    /// Sol's sequence: a lookup starts, Bonjour then answers with IPv4
    /// directly, and the lookup's older answer arrives last.
    func testADirectIPv4AnswerBeatsAnOlderLookup() async throws {
        let lookup = HeldLookup()
        let scanner = NetworkScanner()
        scanner.resolveIPv4 = { lookup.resolve($0) }
        let service = HeldService(name: "PC", hostName: "studio.local.")
        scanner.netServiceBrowser(NetServiceBrowser(), didFind: service, moreComing: false)
        try await Task.sleep(for: .milliseconds(100))
        service.addresses = [ipv6("fe80::1")]
        scanner.netServiceDidResolveAddress(service)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(scanner.hosts.map(\.address), [nil])

        service.addresses = [ipv6("fe80::1"), ipv4("192.168.1.21")]
        scanner.netServiceDidResolveAddress(service)
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.21"])
        lookup.answer("192.168.1.20")
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.168.1.21"], "a tap must reach the current address")
    }

    /// Only a Local Network denial sends the user to Settings. Any other
    /// Bonjour failure keeps Search again.
    func testOnlyALocalNetworkDenialSendsYouToSettings() async throws {
        func problem(_ code: Int) -> NetworkScanner.SearchProblem {
            NetworkScanner.SearchProblem(errorDict: [NetService.errorCode: NSNumber(value: code), NetService.errorDomain: 10])
        }
        // kDNSServiceErr_PolicyDenied and kDNSServiceErr_NoAuth.
        XCTAssertEqual(problem(-65570), .localNetworkOff)
        XCTAssertEqual(problem(-65555), .localNetworkOff)
        // Unknown, busy, bad argument, missing Info.plist entries, no daemon, no router.
        for code in [-72000, -72003, -72004, -72008, -65563, -65566] {
            XCTAssertEqual(problem(code), .couldNotSearch, "\(code)")
        }
        XCTAssertEqual(NetworkScanner.SearchProblem(errorDict: [:]), .couldNotSearch)

        let scanner = NetworkScanner()
        scanner.netServiceBrowser(NetServiceBrowser(), didNotSearch: [NetService.errorCode: -72000, NetService.errorDomain: 10])
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(scanner.searchProblem, .couldNotSearch)
        scanner.netServiceBrowser(NetServiceBrowser(), didNotSearch: [NetService.errorCode: -65570, NetService.errorDomain: 10])
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(scanner.searchProblem, .localNetworkOff)

        let failed = ConnectView.scanEmptyAdvice(.couldNotSearch)
        XCTAssertEqual(failed.title, "Couldn't search for PCs")
        XCTAssertTrue(failed.detail.hasPrefix("Search again, or scan the QR code or type the Address"), failed.detail)
        let blocked = ConnectView.scanEmptyAdvice(.localNetworkOff)
        XCTAssertEqual(blocked.title, "EternalMonitor can't look for PCs")
        XCTAssertEqual(blocked.detail, "Turn on Local Network for EternalMonitor in Settings.")
        XCTAssertNil(ConnectView.scanEmptyAdvice(nil).title)
        XCTAssertTrue(ConnectView.scanEmptyAdvice(nil).detail.hasPrefix("No PCs found."))
    }

    func testSavedPCsStayVisibleUnlessDiscoveryListsTheSameOne() {
        let saved = [
            RecentConnection(host: "192.168.1.20", port: 9876, lastUsed: .now, isUSB: false),
            RecentConnection(host: "100.80.149.7", port: 9876, lastUsed: .now, isUSB: false),
            RecentConnection(host: "Studio PC", port: 9877, lastUsed: .now, isUSB: true),
        ]
        // Someone else's PC on a shared network hides nothing.
        let neighbour = [DiscoveredHost(name: "EternalMonitor (NEIGHBOUR)", address: "192.168.1.44", port: 9876)]
        XCTAssertEqual(RecentConnection.unlisted(saved, discovered: neighbour).map(\.host),
            ["192.168.1.20", "100.80.149.7", "Studio PC"])
        // The user's own PC found again is listed once, under Found.
        let mine = neighbour + [DiscoveredHost(name: "EternalMonitor (STUDIO)", address: "192.168.1.20", port: 9876)]
        XCTAssertEqual(RecentConnection.unlisted(saved, discovered: mine).map(\.host), ["100.80.149.7", "Studio PC"])
        // Same address on another port is a different entry.
        let otherPort = [DiscoveredHost(name: "PC", address: "192.168.1.20", port: 9900)]
        XCTAssertEqual(RecentConnection.unlisted(saved, discovered: otherPort).count, 3)
        // A PC still being found matches nothing.
        let finding = [DiscoveredHost(name: "PC", address: nil, port: 9876, finding: true)]
        XCTAssertEqual(RecentConnection.unlisted(saved, discovered: finding).count, 3)
    }
}

/// A host-name lookup that answers when the test says so, as a slow
/// multicast DNS reply would. One lookup runs at a time.
private final class HeldLookup: @unchecked Sendable {
    private let answered = DispatchSemaphore(value: 0)
    private var address: String?

    func resolve(_ hostName: String) -> String? {
        answered.wait()
        return address
    }

    func answer(_ address: String) {
        self.address = address
        answered.signal()
    }
}

/// A host-name lookup that finds nothing and counts how often it ran.
private final class CountedLookup: @unchecked Sendable {
    private let lock = NSLock()
    private var runs = 0

    var count: Int { lock.withLock { runs } }

    func resolve(_ hostName: String) -> String? {
        lock.withLock { runs += 1 }
        return nil
    }
}

/// A Bonjour service whose resolve answers only when the test calls the
/// delegate method itself.
private final class HeldService: NetService, @unchecked Sendable {
    private let heldHostName: String
    private var heldAddresses: [Data]?
    private(set) var stopped = false

    init(name: String, hostName: String) {
        heldHostName = hostName
        super.init(domain: "local.", type: "_eternaldisplay._udp.", name: name, port: 9876)
    }

    override var addresses: [Data]? {
        get { heldAddresses }
        set { heldAddresses = newValue }
    }
    override var hostName: String? { heldHostName }
    override var port: Int { 9876 }
    override func resolve(withTimeout timeout: TimeInterval) {}
    override func stop() { stopped = true }
}
