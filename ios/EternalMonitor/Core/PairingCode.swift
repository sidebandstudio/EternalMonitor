import Foundation

/// Parses the `eternaldisplay://host:port` pairing links the host renders as
/// QR codes. Pure and unit-tested — the old inline `split(separator: ":")`
/// broke on IPv6 addresses and trailing slashes.
enum PairingCode {
    enum ParseError: Error, Equatable {
        case wrongScheme
        case missingHost
        case missingOrInvalidPort
        case invalidToken
    }

    static let scheme = "eternaldisplay"

    static func parse(_ value: String) -> Result<(host: String, port: UInt16, token: Data?), ParseError> {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let components = URLComponents(string: trimmed),
              components.scheme?.lowercased() == scheme
        else {
            return .failure(.wrongScheme)
        }
        // URLComponents keeps the brackets on IPv6 literals ("[fe80::1]");
        // NWConnection wants the bare address.
        guard var host = components.host, !host.isEmpty else {
            return .failure(.missingHost)
        }
        if host.hasPrefix("["), host.hasSuffix("]") {
            host = String(host.dropFirst().dropLast())
        }
        guard !host.isEmpty else { return .failure(.missingHost) }
        guard let rawPort = components.port, (1...65_535).contains(rawPort) else {
            return .failure(.missingOrInvalidPort)
        }
        let tokens = components.queryItems?.filter { $0.name == "t" } ?? []
        var token: Data?
        if !tokens.isEmpty {
            guard tokens.count == 1, let text = tokens[0].value, text.utf8.count == 32,
                  text.utf8.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }) else {
                return .failure(.invalidToken)
            }
            let characters = Array(text)
            token = Data(stride(from: 0, to: 32, by: 2).compactMap { UInt8(String(characters[$0...$0 + 1]), radix: 16) })
            guard let token, token.contains(where: { $0 != 0 }) else { return .failure(.invalidToken) }
        }
        return .success((host, UInt16(rawPort), token))
    }
}
