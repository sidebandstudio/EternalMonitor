import Foundation
import Network

/// Finding a saved PC again after its address changed: the router gave it a
/// new one, or it moved between a cable and Wi-Fi. The protocol has no PC id,
/// so a PC is the name it gives in its HELLO_ACK, which is also the name in
/// its Bonjour service. Its pairing token then proves it is the same PC.
enum MovedPC {
    /// The Bonjour service a PC named `pcName` advertises.
    static func serviceName(pcName: String) -> String { "EternalMonitor (\(pcName))" }

    /// Whether an attempt at `host` that got no answer may look for its PC
    /// by name. Not an address the user typed, which may be a VPN address
    /// they chose on purpose, and not a host name, which DNS keeps current.
    static func canLookUp(host: String, pcName: String?, typed: Bool) -> Bool {
        guard !typed, let pcName, !pcName.isEmpty else { return false }
        return IPv4Address(host) != nil && host.split(separator: ".").count == 4
    }

    /// Where Bonjour found the PC named `pcName` instead of `host:port`, or
    /// nil: not found, found at the same address, or not one PC. Another PC
    /// with the same name makes Bonjour rename one of the two services, so
    /// any service named for it besides the one found makes it ambiguous.
    static func newAddress(pcName: String, host: String, port: UInt16,
                           found: [DiscoveredHost]) -> (host: String, port: UInt16)? {
        let name = serviceName(pcName: pcName)
        let claims = found.filter { $0.name == name || $0.name.hasPrefix(name + " (") }
        guard claims.count == 1, let pc = claims.first, pc.name == name, let address = pc.address,
              address != host || pc.port != port else { return nil }
        return (address, pc.port)
    }

    /// Whether the saved entry at `host` gives way to the PC's new address:
    /// only a home network address, which the router hands to another
    /// device in time. A VPN or Tailscale address stays for use away from home.
    static func replacesSavedEntry(_ host: String) -> Bool {
        guard host.split(separator: ".").count == 4, let bytes = IPv4Address(host)?.rawValue, bytes.count == 4 else { return false }
        return bytes[0] == 10 || (bytes[0] == 172 && (16...31).contains(bytes[1])) || (bytes[0] == 192 && bytes[1] == 168)
            || (bytes[0] == 169 && bytes[1] == 254)
    }

    /// Looks for EternalMonitor PCs for up to `seconds`, stopping soon after
    /// the service `name` has an address. The wait after it lets a second
    /// service with the same name show up.
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
        let hosts = scanner.hosts
        scanner.stopScan()
        return hosts
    }
}
