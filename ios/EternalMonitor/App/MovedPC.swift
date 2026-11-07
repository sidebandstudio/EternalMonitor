import CryptoKit
import Foundation
import Network
import os

/// Finding a saved PC again after its address changed: the router gave it a
/// new one, or it moved between a cable and Wi-Fi. Only a paired PC that this
/// iPad found with Bonjour, on a home network address that the iPad reaches
/// directly over Wi-Fi or Ethernet, moves by itself; the iPad checks that it
/// is the same PC, at that address, before reconnecting.
enum MovedPC {
    /// The Bonjour service a PC named `pcName` advertises.
    static func serviceName(pcName: String) -> String { "EternalMonitor (\(pcName))" }

    /// Whether an attempt at `host` that got no answer may look for its PC
    /// by name: the PC has a name and a pairing, and its address came from
    /// Bonjour (`found`), not from typing, a QR code or an older version.
    static func canLookUp(host: String, pcName: String?, found: Bool, paired: Bool,
                          homeAddress: (String) -> Bool = isHomeAddress) -> Bool {
        guard found, paired, let pcName, !pcName.isEmpty else { return false }
        return homeAddress(host)
    }

    /// Where Bonjour found the PC named `pcName` instead of `host:port`, or
    /// nil: not found, found at the same address, not on a home network
    /// address, or not one PC. Another PC with the same name makes Bonjour
    /// rename one of the two services, so any other service named for it,
    /// resolved or not, makes it ambiguous.
    static func newAddress(pcName: String, host: String, port: UInt16, found: [DiscoveredHost],
                           homeAddress: (String) -> Bool = isHomeAddress) -> (host: String, port: UInt16)? {
        let name = serviceName(pcName: pcName)
        let claims = found.filter { $0.name == name || $0.name.hasPrefix(name + " (") }
        guard claims.count == 1, let pc = claims.first, pc.name == name, let address = pc.address,
              homeAddress(address), address != host || pc.port != port else { return nil }
        return (address, pc.port)
    }

    /// A home network address, which the router hands out and may hand to
    /// another device in time. VPN and Tailscale addresses never move; a VPN
    /// in these ranges is caught by `isLocal`.
    static func isHomeAddress(_ host: String) -> Bool {
        guard host.split(separator: ".").count == 4, let bytes = IPv4Address(host)?.rawValue, bytes.count == 4 else { return false }
        return bytes[0] == 10 || (bytes[0] == 172 && (16...31).contains(bytes[1])) || (bytes[0] == 192 && bytes[1] == 168)
            || (bytes[0] == 169 && bytes[1] == 254)
    }

    /// Looks for EternalMonitor PCs for up to `seconds`, stopping soon after
    /// the service `name` has an address. The wait after it lets a second
    /// service with the same name show up. Services still resolving count.
    @MainActor
    static func find(_ name: String, seconds: Double = 4) async -> [DiscoveredHost] {
        let scanner = NetworkScanner()
        scanner.retriesWhenEmpty = false
        scanner.startScan()
        let deadline = Date().addingTimeInterval(seconds)
        var foundAt: Date?
        while Date() < deadline {
            try? await Task.sleep(nanoseconds: 250_000_000)
            if foundAt == nil, scanner.hosts.contains(where: { $0.name == name && $0.address != nil }) { foundAt = Date() }
            if let foundAt, Date().timeIntervalSince(foundAt) >= 0.75 { break }
        }
        let claims = scanner.claims
        scanner.stopScan()
        return claims
    }

    /// How the iPad reaches an address: the interface it sends from, whether
    /// the path is Wi-Fi or wired Ethernet and nothing else, and that
    /// interface's IPv4 address and netmask.
    struct Route: Equatable {
        var interface: String
        var direct: Bool
        var address: String
        var netmask: String
    }

    /// Interfaces that are tunnels, whatever the path calls them.
    private static let tunnels = ["utun", "ipsec", "ppp", "tun", "tap", "wg"]

    /// Whether `host` is on the local network `route` sends from: a direct
    /// Wi-Fi or Ethernet path, not a tunnel, with `host` in its subnet. A
    /// VPN that uses home network ranges fails here.
    static func isLocal(_ host: String, over route: Route) -> Bool {
        let ipv4 = { (text: String) -> UInt32? in
            guard text.split(separator: ".").count == 4, let bytes = IPv4Address(text)?.rawValue, bytes.count == 4 else { return nil }
            return bytes.reduce(0) { $0 << 8 | UInt32($1) }
        }
        guard route.direct, !tunnels.contains(where: route.interface.hasPrefix),
              let address = ipv4(route.address), let mask = ipv4(route.netmask), let candidate = ipv4(host),
              mask != 0, mask != .max else { return false }
        return address & mask == candidate & mask
    }

    /// How this iPad reaches `host:port` now, from a UDP path to it that
    /// sends nothing; nil if no path is ready within 2 s.
    static func route(to host: String, port: UInt16) async -> Route? {
        guard let remotePort = NWEndpoint.Port(rawValue: port) else { return nil }
        let connection = NWConnection(to: .hostPort(host: NWEndpoint.Host(host), port: remotePort), using: .udp)
        let queue = DispatchQueue(label: "com.eternal.moved-pc.route")
        let path: NWPath? = await withCheckedContinuation { continuation in
            let done = OSAllocatedUnfairLock(initialState: false)
            let finish: @Sendable (NWPath?) -> Void = { path in
                guard done.withLock({ finished in defer { finished = true }; return !finished }) else { return }
                connection.stateUpdateHandler = nil
                connection.cancel()
                continuation.resume(returning: path)
            }
            connection.stateUpdateHandler = { [weak connection] state in
                switch state {
                case .ready: finish(connection?.currentPath)
                case .failed, .cancelled: finish(nil)
                default: break
                }
            }
            queue.asyncAfter(deadline: .now() + 2) { finish(nil) }
            connection.start(queue: queue)
        }
        guard let path, case .hostPort(.ipv4(let local), _)? = path.localEndpoint,
              var interface = path.availableInterfaces.first?.name else { return nil }
        var direct = (path.usesInterfaceType(.wifi) || path.usesInterfaceType(.wiredEthernet))
            && !path.usesInterfaceType(.other) && !path.usesInterfaceType(.cellular)
        if path.usesInterfaceType(.loopback), let stand = E2E.loopbackRouteInterface {
            interface = stand
            direct = true
        }
        let address = local.rawValue.map { String($0) }.joined(separator: ".")
        guard let netmask = netmask(interface: interface, address: address) else { return nil }
        return Route(interface: interface, direct: direct, address: address, netmask: netmask)
    }

    /// The netmask of `address` on `interface`, from the interface list.
    private static func netmask(interface: String, address: String) -> String? {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return nil }
        defer { freeifaddrs(first) }
        let text = { (pointer: UnsafeMutablePointer<sockaddr>) -> String in
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            getnameinfo(pointer, socklen_t(pointer.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST)
            return String(cString: host)
        }
        for entry in sequence(first: first, next: { $0.pointee.ifa_next }) {
            guard String(cString: entry.pointee.ifa_name) == interface, let addr = entry.pointee.ifa_addr,
                  addr.pointee.sa_family == sa_family_t(AF_INET), text(addr) == address,
                  let mask = entry.pointee.ifa_netmask else { continue }
            return text(mask)
        }
        return nil
    }

    /// What the PC holding `token` answers to `nonce`, receiving at `port`
    /// on `addresses` (4 bytes each).
    static func proof(token: Data, nonce: Data, port: UInt16, addresses: [Data]) -> Data {
        Data(HMAC<SHA256>.authenticationCode(for: WireIdentity.message(nonce: nonce, port: port, addresses: addresses),
                                             using: SymmetricKey(data: token)))
    }

    /// Asks the PC at `host:port`, over `link` and nothing else, to show it
    /// is the PC that `token` paired with, receiving at `host:port`. True
    /// once its answer checks out and names that address; false after
    /// `attempts` requests go without one, as from a PC before 0.3.5, or a
    /// wrong proof. The token itself is never sent.
    static func confirm(host: String, port: UInt16, token: Data, link: any MediaLink, attempts: Int = 8,
                        interval: Double = 0.25) async -> Bool {
        var generator = SystemRandomNumberGenerator()
        let nonce = Data((0..<16).map { _ in UInt8.random(in: .min ... .max, using: &generator) })
        let request = Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .identify(WireIdentify(nonce: nonce)))
        let key = SymmetricKey(data: token)
        let asked = IPv4Address(host)?.rawValue
        let box = LinkBox(link)
        return await withCheckedContinuation { continuation in
            let done = OSAllocatedUnfairLock(initialState: false)
            let finish: @Sendable (Bool) -> Void = { result in
                guard done.withLock({ finished in defer { finished = true }; return !finished }) else { return }
                box.link.controlQueue.async {
                    box.link.onControlDatagram = nil
                    box.link.onListenerReady = nil
                    box.link.onError = nil
                    box.link.stop()
                }
                continuation.resume(returning: result)
            }
            link.onControlDatagram = { data in
                guard case .identity(let identity)? = Wire.parseControl(data)?.message,
                      identity.version == WireIdentify.version, identity.nonce == nonce else { return }
                let signed = WireIdentity.message(nonce: nonce, port: identity.port, addresses: identity.addresses)
                guard HMAC<SHA256>.isValidAuthenticationCode(identity.proof, authenticating: signed, using: key) else {
                    return finish(false)
                }
                // The PC signs where it receives the stream. An answer for
                // another address isn't the saved PC at this one.
                if identity.port == port, let asked, identity.addresses.contains(asked) { finish(true) }
            }
            link.onError = { _ in finish(false) }
            link.onListenerReady = { _ in
                for attempt in 0..<attempts {
                    box.link.controlQueue.asyncAfter(deadline: .now() + interval * Double(attempt)) {
                        if !done.withLock({ $0 }) { box.link.send(request) }
                    }
                }
            }
            // A link that never gets ready ends here too.
            DispatchQueue.global().asyncAfter(deadline: .now() + interval * Double(attempts) + 0.5) { finish(false) }
            if !link.start(host: host) { finish(false) }
        }
    }

    /// The probe's link, handed to the queues that finish it.
    private final class LinkBox: @unchecked Sendable {
        let link: any MediaLink
        init(_ link: any MediaLink) { self.link = link }
    }
}
