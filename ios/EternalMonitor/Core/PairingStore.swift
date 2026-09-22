import Foundation
import Combine
import Security

struct PairingRecord {
    let account: String
    let token: Data
}

protocol PairingStorage {
    func records() throws -> [PairingRecord]
    func save(account: String, token: Data) throws
    func remove(account: String?) throws
}

struct KeychainPairingStorage: PairingStorage {
    static let service = "com.eternal.monitor.pairing"
    struct Failure: Error { let status: OSStatus }
    private var query: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: Self.service]
    }

    func records() throws -> [PairingRecord] {
        var query = query
        query[kSecMatchLimit as String] = kSecMatchLimitAll
        query[kSecReturnAttributes as String] = true
        query[kSecReturnData as String] = true
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return [] }
        guard status == errSecSuccess else { throw Failure(status: status) }
        return (result as? [[String: Any]] ?? []).compactMap { item in
            guard let account = item[kSecAttrAccount as String] as? String,
                  let token = item[kSecValueData as String] as? Data else { return nil }
            return PairingRecord(account: account, token: token)
        }
    }

    func save(account: String, token: Data) throws {
        var query = query
        query[kSecAttrAccount as String] = account
        let values: [String: Any] = [kSecValueData as String: token,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly]
        var status = SecItemUpdate(query as CFDictionary, values as CFDictionary)
        if status == errSecItemNotFound {
            query.merge(values) { _, new in new }
            status = SecItemAdd(query as CFDictionary, nil)
        }
        guard status == errSecSuccess else { throw Failure(status: status) }
    }

    func remove(account: String?) throws {
        var query = query
        if let account { query[kSecAttrAccount as String] = account }
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw Failure(status: status) }
    }
}

@MainActor
final class PairingStore: ObservableObject {
    static let shared = PairingStore(storage: KeychainPairingStorage())
    @Published private(set) var revision = 0
    private let storage: any PairingStorage
    init(storage: any PairingStorage) { self.storage = storage }

    static func address(host: String, port: UInt16) -> String {
        let host = host.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return "\(host.contains(":") ? "[\(host)]" : host):\(port)"
    }

    static func valid(_ token: Data) -> Bool { token.count == 16 && token.contains(where: { $0 != 0 }) }

    func token(address: String) throws -> Data? {
        try storage.records().first { $0.account.hasSuffix("|\(address)") && Self.valid($0.token) }?.token
    }

    func isPaired(host: String, port: UInt16) -> Bool {
        (try? token(address: Self.address(host: host, port: port))) != nil
    }

    func save(token: Data, hostName: String, address: String) throws {
        guard Self.valid(token) else { return }
        let account = "\(hostName)|\(address)"
        try storage.save(account: account, token: token)
        // Replace provisional QR names and previous identities at this address.
        for record in try storage.records() where record.account != account && record.account.hasSuffix("|\(address)") {
            try storage.remove(account: record.account)
        }
        revision += 1
    }

    func remove(address: String) throws {
        for record in try storage.records() where record.account.hasSuffix("|\(address)") {
            try storage.remove(account: record.account)
        }
        revision += 1
    }

    func forgetAll() throws {
        try storage.remove(account: nil)
        revision += 1
    }
}
