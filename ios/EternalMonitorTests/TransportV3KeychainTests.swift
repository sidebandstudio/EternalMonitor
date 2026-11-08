import EternalTransportV3
import Security
import XCTest

/// The transport v3 key store on the real Keychain. It runs here, hosted by
/// the app, because the Keychain needs the app's entitlement; the package's
/// own tests cover the key rule and the stored form on an in-memory store.
final class TransportV3KeychainTests: XCTestCase {
    private let service = "com.eternal.monitor.transport-v3.tests"
    private var hostId: Id16!

    override func setUp() {
        var bytes = [UInt8](repeating: 0, count: 16)
        XCTAssertEqual(SecRandomCopyBytes(kSecRandomDefault, 16, &bytes), errSecSuccess)
        hostId = Id16(bytes)
    }

    override func tearDown() {
        try? KeychainKeyStore(service: service).delete(hostId: hostId)
    }

    private func key(_ byte: UInt8) -> ClientKey {
        ClientKey(devId: Id16([UInt8](repeating: byte, count: 16))!,
                  kDev: SecretBytes(copying: [UInt8](repeating: byte &+ 0x40, count: 32)))!
    }

    private func assertSame(_ x: KeyRing?, _ y: KeyRing, file: StaticString = #filePath, line: UInt = #line) {
        guard let x else { return XCTFail("nothing stored", file: file, line: line) }
        XCTAssertEqual(x.slotId, y.slotId, file: file, line: line)
        XCTAssertEqual(x.lastTimestamp, y.lastTimestamp, file: file, line: line)
        XCTAssertEqual(x.tombstoned, y.tombstoned, file: file, line: line)
        XCTAssertEqual(x.keys.map(\.key.devId), y.keys.map(\.key.devId), file: file, line: line)
        XCTAssertEqual(x.keys.map(\.listed), y.keys.map(\.listed), file: file, line: line)
        for (a, b) in zip(x.keys, y.keys) {
            XCTAssertTrue(a.key.kDev.constantTimeEquals(b.key.kDev), file: file, line: line)
        }
    }

    func testAddUpdateReadBackAndForget() throws {
        let store = KeychainKeyStore(service: service)
        XCTAssertNil(try store.load(hostId: hostId))
        var ring = KeyRing(hostId: hostId, slotId: Id16([UInt8](repeating: 0x5C, count: 16))!)
        XCTAssertTrue(ring.add(key(1)))
        try store.save(ring)
        assertSame(try store.load(hostId: hostId), ring)

        XCTAssertTrue(ring.add(key(2)))
        ring.apply(SlotStatus(held: [(key(2).devId, .pending)], tombstones: [key(1).devId]))
        ring.lastTimestamp = 1_762_500_000_123_456
        try store.save(ring)
        assertSame(try store.load(hostId: hostId), ring)

        try store.delete(hostId: hostId)
        XCTAssertNil(try store.load(hostId: hostId))
        XCTAssertNoThrow(try store.delete(hostId: hostId), "forgetting twice is fine")
    }

    /// One item per host, readable after first unlock, never synced and
    /// never in a backup that restores to another device, on the
    /// data-protection Keychain; the update path keeps all of that.
    func testTheItemsProtection() throws {
        let store = KeychainKeyStore(service: service)
        var ring = KeyRing(hostId: hostId, slotId: Id16([UInt8](repeating: 0x5C, count: 16))!)
        ring.add(key(3))
        try store.save(ring)
        ring.add(key(4))
        try store.save(ring)
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecUseDataProtectionKeychain as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll,
            kSecReturnAttributes as String: true,
        ]
        var result: CFTypeRef?
        XCTAssertEqual(SecItemCopyMatching(query as CFDictionary, &result), errSecSuccess)
        let items = try XCTUnwrap(result as? [[String: Any]]).filter { $0[kSecAttrAccount as String] as? String == hostId.description }
        XCTAssertEqual(items.count, 1)
        let item = try XCTUnwrap(items.first)
        XCTAssertEqual(item[kSecAttrAccessible as String] as? String, kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
        XCTAssertNotEqual(item[kSecAttrSynchronizable as String] as? Bool, true)
    }

    func testAnItemForAnotherHostIsNeverReturned() throws {
        let store = KeychainKeyStore(service: service)
        try store.save(KeyRing(hostId: hostId, slotId: Id16([UInt8](repeating: 0x5C, count: 16))!))
        let other = Id16([UInt8](repeating: 0xEE, count: 16))!
        XCTAssertNil(try store.load(hostId: other))
    }
}
