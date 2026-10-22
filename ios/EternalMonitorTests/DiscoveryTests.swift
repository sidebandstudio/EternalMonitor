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

    /// A PC with WSL, Hyper-V or a VPN answers with those adapters'
    /// addresses too. The iPad must pick the one on its own network.
    func testTheAddressOnThisIPadsNetworkIsChosen() {
        let home = [LocalNetwork.Subnet("192.168.1.34", mask: "255.255.255.0")!]
        let answers = [ipv4("172.21.0.1"), ipv4("100.80.1.2"), ipv4("192.168.1.20")]
        XCTAssertEqual(DiscoveredHost.resolved(name: "PC", port: 9876, addresses: answers, local: home).address, "192.168.1.20")
        // Not on this iPad's network: a private address before others.
        XCTAssertEqual(DiscoveredHost.resolved(name: "PC", port: 9876,
            addresses: [ipv4("100.80.1.2"), ipv4("10.0.0.5")], local: home).address, "10.0.0.5")
        XCTAssertEqual(DiscoveredHost.resolved(name: "PC", port: 9876, addresses: [ipv4("100.80.1.2")], local: []).address, "100.80.1.2")
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
