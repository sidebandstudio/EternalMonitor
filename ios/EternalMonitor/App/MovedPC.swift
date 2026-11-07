import CryptoKit
import Foundation
import Network
import os

/// Finding a saved PC again after its address changed: the router gave it a
/// new one, or it moved between a cable and Wi-Fi. Only a paired PC that this
/// iPad found with Bonjour, on a home network address, moves by itself; the
/// iPad checks that it is the same PC before reconnecting.
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
    /// another device in time. VPN and Tailscale addresses never move.
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

    /// What the PC holding `token` answers to `nonce`.
    static func proof(token: Data, nonce: Data) -> Data {
        Data(HMAC<SHA256>.authenticationCode(for: nonce + WireIdentify.context, using: SymmetricKey(data: token)))
    }

    /// Asks the PC at `host:port`, over `link` and nothing else, to show it
    /// is the PC that `token` paired with. True once its answer checks out;
    /// false after `attempts` requests go unanswered, as from a PC before
    /// 0.3.5, or a wrong answer. The token itself is never sent.
    static func confirm(host: String, token: Data, link: any MediaLink, attempts: Int = 8,
                        interval: Double = 0.25) async -> Bool {
        var generator = SystemRandomNumberGenerator()
        let nonce = Data((0..<16).map { _ in UInt8.random(in: .min ... .max, using: &generator) })
        let request = Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .identify(WireIdentify(nonce: nonce)))
        let key = SymmetricKey(data: token)
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
                finish(HMAC<SHA256>.isValidAuthenticationCode(identity.proof, authenticating: nonce + WireIdentify.context,
                                                               using: key))
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
