import Foundation

/// What the iPad holds for one host: its slot, its keys, what the most
/// recent authenticated slot status said about each, the keys statuses have
/// tombstoned, and the last message 1 timestamp. The iPad key rule of
/// section 4.3:
///
/// - A key is deleted only when an authenticated status names it in a
///   tombstone (`apply`), when a sealed `PairRefused` ends the exchange
///   that created it (`discardRefusedPairing`), or when the user picks
///   "Forget this PC" (`KeyStore.delete`). Silence never deletes anything,
///   and neither does a key being absent from a status.
/// - A tombstoned key is never used again: it leaves `keys` at once, and its
///   `dev_id` is remembered (the last 32), so a later `add` of it is
///   refused. That holds in memory even if saving fails; the next save that
///   succeeds makes it durable, since every save writes the whole ring.
///   If the app restarts before any save succeeds, the stored ring still
///   holds the key. That is accepted, not blocked: the host decides, a
///   retired `dev_id` is never valid again, so the key's handshakes go
///   unanswered, and the next status tombstones it again (while it is
///   among the slot's last 8). The cost is time spent on that key, never
///   access. Blocking connects until a save succeeds would turn a failing
///   Keychain into no reconnects at all.
/// - Connect order (`connectOrder`): keys the most recent status listed as
///   pending, then the one it listed as active, then keys it didn't list,
///   newest first within each group. A stale status only changes the order.
///   A key an earlier status listed but the most recent one doesn't is in
///   the last group: nothing about it is known now, and it's kept.
/// - Finalizing promptly (`keyToFinalize`): a held key the last status
///   listed as pending is worth a reconnect straight away.
///
/// A value type with no I/O. The caller saves it through a `KeyStore` after
/// every change, before the next message goes out.
public struct KeyRing {
    /// Keys held at once: the host lists at most 4 per slot, and keys no
    /// status mentions are kept until "Forget this PC".
    public static let maxKeys = 16
    /// Tombstoned `dev_id`s remembered, oldest dropped first. The host keeps
    /// the last 8 per slot.
    public static let maxRemembered = 32

    public struct HeldKey {
        public let key: ClientKey
        /// What the most recent status said about it; nil if it didn't list it.
        public fileprivate(set) var listed: RecordState?
    }

    public let hostId: Id16
    /// The iPad's place on this host. Made once, at the first pairing with
    /// `hostId`, and reused for every pairing with it after.
    public let slotId: Id16
    /// Oldest first.
    public private(set) var keys: [HeldKey] = []
    /// Oldest first.
    public private(set) var tombstoned: [Id16] = []
    /// The last message 1 timestamp sent to this host (`Client.lastTimestamp`).
    public var lastTimestamp: UInt64 = 0

    public init(hostId: Id16, slotId: Id16) {
        self.hostId = hostId
        self.slotId = slotId
    }

    /// A ring for a host this iPad has never paired with: a new random
    /// `slot_id`. Pairing (a later phase) is the only caller.
    public static func fresh(hostId: Id16, random: RandomSource = SystemRandom()) -> KeyRing {
        var slot = [UInt8](repeating: 0, count: 16)
        random.fill(&slot)
        return KeyRing(hostId: hostId, slotId: Id16(slot)!)
    }

    public enum AddOutcome: Equatable, Sendable {
        case added
        case alreadyHeld
        /// A status tombstoned this `dev_id`: it is never used again.
        case tombstoned
        /// The ring holds `maxKeys` keys. Nothing is evicted: a key no
        /// status lists may be a pairing whose `PairDone` was lost, which
        /// section 4.3 keeps until "Forget this PC". The pairing sends
        /// `PairAbort`, and the iPad offers Forget, then pairing again.
        case full
    }

    /// Adds a key from a pairing, before its `PairConfirm` goes out (store
    /// first, section 4.1 step 3). It joins the group no status has listed.
    /// Anything but `.added` changes nothing.
    public mutating func add(_ key: ClientKey) -> AddOutcome {
        if tombstoned.contains(key.devId) { return .tombstoned }
        if keys.contains(where: { $0.key.devId == key.devId }) { return .alreadyHeld }
        if keys.count >= Self.maxKeys { return .full }
        keys.append(HeldKey(key: key, listed: nil))
        return .added
    }

    /// Applies an authenticated slot status: every held key it tombstones is
    /// deleted and remembered, and every other key's listing becomes what
    /// this status says. Returns the `dev_id`s deleted.
    @discardableResult
    public mutating func apply(_ status: SlotStatus) -> [Id16] {
        var deleted: [Id16] = []
        for devId in status.tombstones {
            if let index = keys.firstIndex(where: { $0.key.devId == devId }) {
                keys.remove(at: index)
                deleted.append(devId)
                remember(devId)
            }
        }
        for index in keys.indices {
            keys[index].listed = status.state(of: keys[index].key.devId)
        }
        return deleted
    }

    /// A sealed `PairRefused` ended the exchange that created this key.
    public mutating func discardRefusedPairing(_ devId: Id16) {
        keys.removeAll { $0.key.devId == devId }
    }

    /// The keys to try on a connect, in order, leaving out `excluding` (for
    /// example keys the host answered `PairAgain` for).
    public func connectOrder(excluding: Set<Id16> = []) -> [ClientKey] {
        let newestFirst = keys.reversed().filter { !excluding.contains($0.key.devId) }
        return newestFirst.filter { $0.listed == .pending }.map(\.key)
            + newestFirst.filter { $0.listed == .active }.map(\.key)
            + newestFirst.filter { $0.listed == nil }.map(\.key)
    }

    /// A held key the most recent status listed as pending, other than the
    /// live session's: reconnect under it now, so the host finalizes it.
    public func keyToFinalize(liveDevId: Id16?) -> ClientKey? {
        keys.reversed().first { $0.listed == .pending && $0.key.devId != liveDevId }?.key
    }

    /// The key the most recent status listed as the slot's active one: the
    /// key a re-pairing's replace proof uses.
    public var believedActive: ClientKey? {
        keys.first { $0.listed == .active }?.key
    }

    public func holds(_ devId: Id16) -> Bool {
        keys.contains { $0.key.devId == devId }
    }

    private mutating func remember(_ devId: Id16) {
        guard !tombstoned.contains(devId) else { return }
        if tombstoned.count >= Self.maxRemembered {
            tombstoned.removeFirst()
        }
        tombstoned.append(devId)
    }
}

/// "When every key is silent": walks the connect order one attempt at a
/// time, and after 3 full cycles without a session suggests pairing again.
/// Nothing is deleted. Make a new one for each connect episode; the order
/// is read from the ring at every step, so a status that arrives meanwhile
/// changes only what comes next.
public struct KeyCycle {
    public static let cyclesBeforeSuggesting = 3

    private var triedThisCycle: Set<Id16> = []
    public private(set) var completedCycles = 0

    public init() {}

    /// The next key to try, or nil if the ring has none to offer.
    public mutating func nextKey(in ring: KeyRing, excluding: Set<Id16> = []) -> ClientKey? {
        let order = ring.connectOrder(excluding: excluding)
        guard !order.isEmpty else { return nil }
        if let key = order.first(where: { !triedThisCycle.contains($0.devId) }) {
            triedThisCycle.insert(key.devId)
            return key
        }
        completedCycles += 1
        triedThisCycle = [order[0].devId]
        return order[0]
    }

    public var suggestsPairingAgain: Bool {
        completedCycles >= Self.cyclesBeforeSuggesting
    }
}

// MARK: - Stored form

/// The ring's bytes, one Keychain item per `host_id`:
///
///     "EMV3" | format 1 | host_id 16 | slot_id 16 | last_timestamp u64
///     | key_count u8 (≤ 16) | key_count × (dev_id 16 | K_dev 32 | listed u8)
///     | tombstone_count u8 (≤ 32) | tombstone_count × dev_id 16
///
/// `listed` is 0 pending, 1 active, 0xFF not listed. Decoding is exact:
/// anything this code never writes (other lengths, values or counts, a
/// duplicate `dev_id`, two active, a timestamp at or above 2^62) is
/// rejected, never repaired.
extension KeyRing {
    static let magic = Array("EMV3".utf8)
    static let format: UInt8 = 1
    private static let notListed: UInt8 = 0xFF

    /// The stored form, in wiped-on-release storage.
    func encoded() -> SecretBytes {
        let count = 4 + 1 + 16 + 16 + 8 + 1 + keys.count * 49 + 1 + tombstoned.count * 16
        return SecretBytes(count: count) { out in
            var at = 0
            func put<Bytes: Collection>(_ bytes: Bytes) where Bytes.Element == UInt8 {
                for byte in bytes {
                    out[at] = byte
                    at += 1
                }
            }
            put(Self.magic)
            put([Self.format])
            put(hostId.bytes)
            put(slotId.bytes)
            withUnsafeBytes(of: lastTimestamp.littleEndian) { put($0) }
            put([UInt8(keys.count)])
            for held in keys {
                put(held.key.devId.bytes)
                held.key.kDev.withUnsafeBytes { put($0) }
                put([held.listed?.rawValue ?? Self.notListed])
            }
            put([UInt8(tombstoned.count)])
            for devId in tombstoned {
                put(devId.bytes)
            }
            precondition(at == count)
        }
    }

    static func decode(_ bytes: UnsafeRawBufferPointer) throws -> KeyRing {
        var at = 0
        func take(_ count: Int) throws -> UnsafeRawBufferPointer {
            guard count <= bytes.count - at else { throw KeyStoreError.corrupt }
            defer { at += count }
            return UnsafeRawBufferPointer(rebasing: bytes[at..<at + count])
        }
        func id() throws -> Id16 { Id16(try take(16))! }
        guard Array(try take(4)) == magic, try take(1)[0] == format else { throw KeyStoreError.corrupt }
        var ring = KeyRing(hostId: try id(), slotId: try id())
        let timestamp = le64(Array(try take(8)), 0)
        guard timestamp < V3.maxTimestampUs else { throw KeyStoreError.corrupt }
        ring.lastTimestamp = timestamp
        let keyCount = Int(try take(1)[0])
        guard keyCount <= maxKeys else { throw KeyStoreError.corrupt }
        for _ in 0..<keyCount {
            let devId = try id()
            let kDev = SecretBytes(copying: try take(32))
            let listedByte = try take(1)[0]
            let listed: RecordState?
            if listedByte == notListed {
                listed = nil
            } else if let state = RecordState(rawValue: listedByte) {
                listed = state
            } else {
                throw KeyStoreError.corrupt
            }
            ring.keys.append(HeldKey(key: ClientKey(devId: devId, kDev: kDev)!, listed: listed))
        }
        let tombCount = Int(try take(1)[0])
        guard tombCount <= maxRemembered else { throw KeyStoreError.corrupt }
        for _ in 0..<tombCount {
            ring.tombstoned.append(try id())
        }
        let all = ring.keys.map(\.key.devId) + ring.tombstoned
        guard at == bytes.count,
              Set(all).count == all.count,
              ring.keys.filter({ $0.listed == .active }).count <= 1
        else { throw KeyStoreError.corrupt }
        return ring
    }
}
