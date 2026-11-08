import Foundation
import Security

public enum KeyStoreError: Error, Equatable {
    /// The Keychain answered with this status.
    case keychain(OSStatus)
    /// The stored bytes don't decode. Reported, never overwritten or
    /// repaired: the caller decides (for example "Forget this PC").
    case corrupt
}

/// Where the iPad keeps its `KeyRing`s, one per `host_id`. Each `save`
/// replaces the host's whole ring atomically.
public protocol KeyStore: AnyObject {
    func load(hostId: Id16) throws -> KeyRing?
    func save(_ ring: KeyRing) throws
    /// "Forget this PC": the keys and the `slot_id` go together, so a later
    /// pairing is a fresh one in a new slot (section 4.3, "An iPad that lost
    /// its Keychain").
    func delete(hostId: Id16) throws
}

/// The Keychain store: one generic-password item per host, service
/// `com.eternal.monitor.transport-v3`, account the `host_id` in hex.
///
/// - `AfterFirstUnlockThisDeviceOnly`: readable for a reconnect while the
///   iPad is locked after its first unlock, never in a backup that restores
///   to another device, never synced.
/// - The data-protection Keychain on every platform (on macOS it needs a
///   signed app with a keychain access group).
/// - `save` adds the item, or on `errSecDuplicateItem` updates its data and
///   protection class. Either is one atomic Keychain write.
/// - The bytes go to the Keychain without a copy of ours (`bytesNoCopy`
///   over the wiped-on-release encoding). What the Keychain hands back on
///   `load` is a `CFData` we can't wipe; it's decoded into `SecretBytes`
///   and released at once.
public final class KeychainKeyStore: KeyStore {
    public static let defaultService = "com.eternal.monitor.transport-v3"

    private let service: String

    /// `service` is for tests, which use their own.
    public init(service: String = KeychainKeyStore.defaultService) {
        self.service = service
    }

    private func query(_ hostId: Id16) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: hostId.description,
            kSecAttrSynchronizable as String: false,
            kSecUseDataProtectionKeychain as String: true,
        ]
    }

    public func load(hostId: Id16) throws -> KeyRing? {
        var request = query(hostId)
        request[kSecReturnData as String] = true
        request[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(request as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else { throw KeyStoreError.keychain(status) }
        guard let data = result as? Data else { throw KeyStoreError.corrupt }
        let ring = try data.withUnsafeBytes { try KeyRing.decode($0) }
        guard ring.hostId == hostId else { throw KeyStoreError.corrupt }
        return ring
    }

    public func save(_ ring: KeyRing) throws {
        let blob = ring.encoded()
        try blob.withUnsafeBytes { bytes in
            let data = Data(bytesNoCopy: UnsafeMutableRawPointer(mutating: bytes.baseAddress!),
                            count: bytes.count, deallocator: .none)
            let values: [String: Any] = [
                kSecValueData as String: data,
                kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            ]
            var status = SecItemAdd(query(ring.hostId).merging(values) { $1 } as CFDictionary, nil)
            if status == errSecDuplicateItem {
                status = SecItemUpdate(query(ring.hostId) as CFDictionary, values as CFDictionary)
            }
            guard status == errSecSuccess else { throw KeyStoreError.keychain(status) }
        }
    }

    public func delete(hostId: Id16) throws {
        let status = SecItemDelete(query(hostId) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeyStoreError.keychain(status)
        }
    }
}
