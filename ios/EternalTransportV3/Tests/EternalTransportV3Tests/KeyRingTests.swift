import Security
import XCTest
@testable import EternalTransportV3

/// The iPad key rule (section 4.3) and the stores behind it.
final class KeyRingTests: XCTestCase {
    let hostId = id16(0xB2)

    func makeRing(_ keys: [ClientKey]) -> KeyRing {
        var ring = KeyRing(hostId: hostId, slotId: id16(0x5C))
        for key in keys {
            XCTAssertEqual(ring.add(key), .added)
        }
        return ring
    }

    func ids(_ keys: [ClientKey]) -> [Id16] { keys.map(\.devId) }

    let a = clientKey(0xA1, 1)
    let b = clientKey(0xB1, 2)
    let c = clientKey(0xC1, 3)
    let d = clientKey(0xD1, 4)

    func testOnlyATombstoneDeletesAndADeletedKeyIsNeverAddedAgain() {
        var ring = makeRing([a, b, c])
        XCTAssertEqual(ring.apply(SlotStatus(held: [(a.devId, .active)])), [], "absence deletes nothing")
        XCTAssertEqual(ring.keys.count, 3)
        XCTAssertEqual(ring.apply(SlotStatus(held: [(a.devId, .active)], tombstones: [b.devId, id16(0xEE)])), [b.devId])
        XCTAssertFalse(ring.holds(b.devId))
        XCTAssertEqual(ring.tombstoned, [b.devId])
        XCTAssertEqual(ring.add(b), .tombstoned, "a tombstoned key is never used again")
        XCTAssertEqual(ring.add(a), .alreadyHeld)
        XCTAssertFalse(ring.connectOrder().contains { $0.devId == b.devId })
        // A late copy of an older status changes the order, never what's kept.
        XCTAssertEqual(ring.apply(SlotStatus(held: [(b.devId, .pending), (c.devId, .active)])), [])
        XCTAssertFalse(ring.holds(b.devId))
        XCTAssertEqual(ids(ring.connectOrder()), [c.devId, a.devId])
    }

    func testConnectOrderIsPendingThenActiveThenUnlistedNewestFirst() {
        var ring = makeRing([a, b, c, d])
        XCTAssertEqual(ids(ring.connectOrder()), [d.devId, c.devId, b.devId, a.devId], "newest first")
        ring.apply(SlotStatus(held: [(a.devId, .pending), (b.devId, .active), (c.devId, .pending)]))
        XCTAssertEqual(ids(ring.connectOrder()), [c.devId, a.devId, b.devId, d.devId])
        XCTAssertEqual(ids(ring.connectOrder(excluding: [c.devId])), [a.devId, b.devId, d.devId])
        XCTAssertEqual(ring.believedActive?.devId, b.devId)
        XCTAssertEqual(ring.keyToFinalize(liveDevId: b.devId)?.devId, c.devId)
        XCTAssertEqual(ring.keyToFinalize(liveDevId: c.devId)?.devId, a.devId)
        // The most recent status decides; one that lists nothing makes every
        // key unlisted, and nothing is deleted.
        ring.apply(SlotStatus())
        XCTAssertEqual(ids(ring.connectOrder()), [d.devId, c.devId, b.devId, a.devId])
        XCTAssertNil(ring.keyToFinalize(liveDevId: nil))
        XCTAssertNil(ring.believedActive)
    }

    func testTheRingIsBounded() {
        var ring = makeRing([])
        for byte in 0..<UInt8(KeyRing.maxKeys) {
            XCTAssertEqual(ring.add(clientKey(byte, byte)), .added)
        }
        XCTAssertEqual(ring.add(clientKey(0xFF, 0xFF)), .full, "nothing is evicted")
        XCTAssertEqual(ring.keys.count, KeyRing.maxKeys)
        XCTAssertFalse(ring.holds(id16(0xFF)))
        var tombstones = (0..<UInt8(KeyRing.maxKeys)).map(id16)
        while !tombstones.isEmpty {
            ring.apply(SlotStatus(tombstones: Array(tombstones.prefix(8))))
            tombstones.removeFirst(min(8, tombstones.count))
        }
        XCTAssertEqual(ring.keys.count, 0)
        XCTAssertEqual(ring.tombstoned.count, 16)
        for byte in 0x40..<UInt8(0x40 + KeyRing.maxKeys) {
            XCTAssertEqual(ring.add(clientKey(byte, byte)), .added)
        }
        ring.apply(SlotStatus(tombstones: (0x40..<UInt8(0x48)).map(id16)))
        ring.apply(SlotStatus(tombstones: (0x48..<UInt8(0x50)).map(id16)))
        XCTAssertEqual(ring.tombstoned.count, KeyRing.maxRemembered)
        ring.apply(SlotStatus())
        XCTAssertEqual(ring.add(clientKey(0x60, 1)), .added)
        XCTAssertEqual(ring.add(clientKey(0x61, 1)), .added)
        ring.apply(SlotStatus(tombstones: [id16(0x60)]))
        XCTAssertEqual(ring.tombstoned.count, KeyRing.maxRemembered, "the oldest is dropped")
        XCTAssertEqual(ring.tombstoned.first, id16(1))
    }

    func testARefusedPairingIsDiscarded() {
        var ring = makeRing([a, b])
        ring.discardRefusedPairing(b.devId)
        XCTAssertEqual(ids(ring.connectOrder()), [a.devId])
    }

    func testAfterThreeSilentCyclesPairingAgainIsSuggested() {
        var ring = makeRing([a, b])
        var cycle = KeyCycle()
        var tried: [Id16] = []
        for _ in 0..<6 {
            XCTAssertFalse(cycle.suggestsPairingAgain)
            tried.append(cycle.nextKey(in: ring)!.devId)
        }
        XCTAssertEqual(tried, [b.devId, a.devId, b.devId, a.devId, b.devId, a.devId])
        XCTAssertEqual(cycle.completedCycles, 2)
        _ = cycle.nextKey(in: ring)
        XCTAssertTrue(cycle.suggestsPairingAgain, "after 3 full cycles; the keys are kept")
        XCTAssertEqual(ring.keys.count, 2)
        // A status that arrives meanwhile changes what comes next.
        ring.apply(SlotStatus(held: [(a.devId, .pending)]))
        var fresh = KeyCycle()
        XCTAssertEqual(fresh.nextKey(in: ring)?.devId, a.devId)
        XCTAssertEqual(fresh.nextKey(in: ring, excluding: [b.devId])?.devId, a.devId, "only a: a new cycle")
        XCTAssertNil(fresh.nextKey(in: KeyRing(hostId: hostId, slotId: id16(1))))
    }

    func testAFreshRingHasARandomSlot() {
        let one = KeyRing.fresh(hostId: hostId, random: SeededRandom(1))
        let two = KeyRing.fresh(hostId: hostId, random: SeededRandom(2))
        XCTAssertNotEqual(one.slotId, two.slotId)
        XCTAssertEqual(one.hostId, hostId)
    }

    // MARK: - Stored form

    func storedRing() -> KeyRing {
        var ring = makeRing([a, b, c])
        ring.apply(SlotStatus(held: [(a.devId, .active), (b.devId, .pending)], tombstones: [d.devId]))
        ring.lastTimestamp = 1_762_500_000_123_456
        return ring
    }

    func assertSame(_ x: KeyRing, _ y: KeyRing, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(x.hostId, y.hostId, file: file, line: line)
        XCTAssertEqual(x.slotId, y.slotId, file: file, line: line)
        XCTAssertEqual(x.lastTimestamp, y.lastTimestamp, file: file, line: line)
        XCTAssertEqual(x.tombstoned, y.tombstoned, file: file, line: line)
        XCTAssertEqual(x.keys.map(\.key.devId), y.keys.map(\.key.devId), file: file, line: line)
        XCTAssertEqual(x.keys.map(\.listed), y.keys.map(\.listed), file: file, line: line)
        XCTAssertEqual(x.keys.map { bytes($0.key.kDev) }, y.keys.map { bytes($0.key.kDev) }, file: file, line: line)
    }

    func testTheStoredFormRoundTripsAndRejectsEveryDamage() throws {
        var ring = storedRing()
        ring.apply(SlotStatus(tombstones: [c.devId]))
        let encoded = bytes(ring.encoded())
        XCTAssertEqual(encoded.count, 4 + 1 + 32 + 8 + 1 + 2 * 49 + 1 + 16)
        let decoded = try encoded.withUnsafeBytes { try KeyRing.decode($0) }
        assertSame(decoded, ring)
        for length in 0..<encoded.count {
            XCTAssertThrowsError(try Array(encoded[..<length]).withUnsafeBytes { try KeyRing.decode($0) }, "length \(length)")
        }
        XCTAssertThrowsError(try (encoded + [0]).withUnsafeBytes { try KeyRing.decode($0) })
        // Flipping a header, count, listing or tombstone byte is rejected;
        // only the ids, keys and timestamp bytes may change to another
        // valid ring.
        for at in 0..<encoded.count {
            var bad = encoded
            bad[at] ^= 0x80
            let result = Result { try bad.withUnsafeBytes { try KeyRing.decode($0) } }
            if case .success(let other) = result {
                XCTAssertNotEqual(bytes(other.encoded()), encoded, "byte \(at)")
            }
        }
        var twoActive = makeRing([a, b])
        twoActive.apply(SlotStatus(held: [(a.devId, .active)]))
        var blob = bytes(twoActive.encoded())
        let secondListing: Int = 46 + 49 + 48 // header, first key, second key's id and K_dev
        blob[secondListing] = RecordState.active.rawValue
        XCTAssertThrowsError(try blob.withUnsafeBytes { try KeyRing.decode($0) })
    }

    func testAFailedSaveStillNeverUsesATombstonedKey() throws {
        let store = InMemoryKeyStore()
        var ring = makeRing([a, b])
        try store.save(ring)
        ring.apply(SlotStatus(tombstones: [a.devId]))
        store.failure = .keychain(errSecIO)
        XCTAssertThrowsError(try store.save(ring))
        XCTAssertEqual(ids(ring.connectOrder()), [b.devId], "in memory, the key is gone at once")
        XCTAssertTrue(try XCTUnwrap(store.load(hostId: hostId)).holds(a.devId), "not yet durable")
        store.failure = nil
        try store.save(ring)
        let loaded = try XCTUnwrap(store.load(hostId: hostId))
        XCTAssertFalse(loaded.holds(a.devId))
        XCTAssertEqual(loaded.tombstoned, [a.devId])
    }

    func testForgetDeletesKeysAndSlot() throws {
        let store = InMemoryKeyStore()
        try store.save(storedRing())
        try store.delete(hostId: hostId)
        XCTAssertNil(try store.load(hostId: hostId))
    }

    func testADamagedItemIsReportedNotRepaired() throws {
        let store = InMemoryKeyStore()
        try store.save(storedRing())
        var damaged = try XCTUnwrap(store.storedBytes(hostId: hostId))
        damaged[4] = 9
        store.setStoredBytes(damaged, hostId: hostId)
        XCTAssertThrowsError(try store.load(hostId: hostId)) { XCTAssertEqual($0 as? KeyStoreError, .corrupt) }
        XCTAssertEqual(store.storedBytes(hostId: hostId), damaged)
    }
}
