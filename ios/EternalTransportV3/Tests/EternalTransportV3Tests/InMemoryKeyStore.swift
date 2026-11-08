import Foundation
@testable import EternalTransportV3

/// An in-memory `KeyStore` for tests: it keeps each ring in its stored form,
/// so every save and load goes through the same encoding as the Keychain,
/// and it can be told to fail.
final class InMemoryKeyStore: KeyStore {
    private var items: [Id16: SecretBytes] = [:]
    /// While set, `save` and `delete` throw this and change nothing.
    var failure: KeyStoreError?

    init() {}

    func load(hostId: Id16) throws -> KeyRing? {
        try items[hostId]?.withUnsafeBytes { try KeyRing.decode($0) }
    }

    func save(_ ring: KeyRing) throws {
        if let failure { throw failure }
        items[ring.hostId] = ring.encoded()
    }

    func delete(hostId: Id16) throws {
        if let failure { throw failure }
        items[hostId] = nil
    }

    /// The stored bytes, for tests that damage them.
    func storedBytes(hostId: Id16) -> [UInt8]? {
        items[hostId]?.withUnsafeBytes { Array($0) }
    }

    func setStoredBytes(_ bytes: [UInt8], hostId: Id16) {
        items[hostId] = SecretBytes(copying: bytes)
    }
}
