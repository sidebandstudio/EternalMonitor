import CryptoKit
import Foundation

/// Tells a PC whose EternalMonitor is older than 0.3.4 from a current one,
/// using only what PCs already send.
///
/// - Bonjour: every PC puts its version in the TXT record's `version`.
///   Only a plain release version counts; a beta's or a missing one doesn't.
/// - HELLO_ACK: every 0.3.4 and later PC sets `hostCapContiguousFrameSeq` in
///   every reply that accepts a session, whatever its hardware or settings,
///   and no earlier PC ever set that bit. This covers PCs reached over USB or
///   by address. Refusals and not-streaming replies may leave every cap out,
///   so they count for nothing either way.
///
/// A PC is old only when nothing it sent says it is current.
enum OldPC {
    static let current = AppVersion("0.3.4")!

    /// True for an old PC, false for a current one, nil when it can't tell.
    /// `hostCaps` must come from a reply that accepted a session.
    static func isOld(announced: String?, hostCaps: UInt16?) -> Bool? {
        if let hostCaps, hostCaps & HelloAck.hostCapContiguousFrameSeq != 0 { return false }
        if let version = announced.flatMap(AppVersion.init) { return version < current }
        return hostCaps == nil ? nil : true
    }

    /// The name a PC gives itself, from its Bonjour service name, with
    /// Bonjour's collision number if it has one: "Lab (2)".
    static func pcName(bonjour name: String) -> String {
        let prefix = "EternalMonitor ("
        guard name.hasPrefix(prefix), name.hasSuffix(")") else { return name }
        var inner = name.dropFirst(prefix.count).dropLast()
        var number = ""
        if let open = inner.range(of: ") (", options: .backwards) {
            let digits = inner[open.upperBound...]
            if !digits.isEmpty, digits.allSatisfy({ $0.isASCII && $0.isNumber }) {
                number = " (\(digits))"
                inner = inner[..<open.lowerBound]
            }
        }
        return inner.isEmpty ? name : String(inner) + number
    }
}

/// The Connect screen's card asking to update an old PC. It shows at most
/// once per PC every three days, only on the Connect screen, and goes away
/// when closed or when a stream starts.
///
/// A PC is known by its pairing token (hashed) when it has one, otherwise by
/// its Bonjour service name, which is unique on a network, and last by the
/// name it answers with. A session at an address links the service the user
/// tapped there, or the one the search lists there right now, to the PC that
/// answered. What a session showed about a PC outlasts the session: a PC
/// that proved current gets no card from a later Bonjour record. Links only
/// ever keep a card away, and never take a service, or a proof, from another
/// PC.
///
/// Over USB, or at an address the search doesn't list, a session only has
/// the PC's name, and no PC sends anything in Bonjour that ties a service to
/// its pairing. Such a session links the one service the search lists as
/// "EternalMonitor (<that name>)", with or without a collision suffix such as
/// " (2)", for this launch only, and nothing while the search lists two or
/// more. Known limit: two PCs with one name on a network, one of them used
/// over USB, can get one extra or one missed card per PC every three days.
@MainActor
final class OldPCPrompt: ObservableObject {
    static let shared = OldPCPrompt()
    static let interval: TimeInterval = 3 * 86_400
    static let defaultsKey = "oldPCPromptMemory"
    /// PCs remembered.
    static let kept = 16

    struct Card: Equatable {
        /// Who the PC is to this memory.
        let key: String
        /// The name shown.
        let pc: String
        /// The release version it announced, if any.
        let version: String?
    }

    struct Memory: Codable, Equatable {
        struct PC: Codable, Equatable {
            var shownAt: Date?
            /// What a session's accepted HELLO_ACK said: current or not.
            var current: Bool?
            var seenAt: Date
        }
        var pcs: [String: PC] = [:]
        /// Bonjour service name to the PC a session through it answered as.
        var services: [String: String] = [:]
    }

    /// The card on screen.
    @Published private(set) var card: Card?
    /// An old PC waiting for the Connect screen.
    private var waiting: Card?
    /// The services the search lists now.
    private var listed: [DiscoveredHost] = []
    /// The service the user tapped, and its address.
    private var tapped: (service: String, address: String)?
    /// This launch's sessions that no address linked, by PC name.
    private var unlinked: [String: String] = [:]
    /// Services linked by those sessions' names, for this launch only.
    private var named: [String: String] = [:]
    private let defaults: UserDefaults
    private let now: () -> Date
    private let log: (String) -> Void

    init(defaults: UserDefaults = .standard, now: @escaping () -> Date = { Date() },
         log: @escaping (String) -> Void = { SupportDiagnostics.shared.record("info", "update", $0) }) {
        self.defaults = defaults
        self.now = now
        self.log = log
    }

    var memory: Memory {
        defaults.data(forKey: Self.defaultsKey).flatMap { try? JSONDecoder().decode(Memory.self, from: $0) } ?? Memory()
    }

    private func save(_ memory: Memory) {
        var memory = memory
        let dropped = memory.pcs.sorted { $0.value.seenAt > $1.value.seenAt }.dropFirst(Self.kept).map(\.key)
        for key in dropped { memory.pcs[key] = nil }
        // Links to the PCs seen last, a few per PC at most.
        let linked = memory.services.filter { memory.pcs[$0.value] != nil }
            .sorted { (memory.pcs[$0.value]!.seenAt, $1.key) > (memory.pcs[$1.value]!.seenAt, $0.key) }
        memory.services = Dictionary(uniqueKeysWithValues: linked.prefix(Self.kept * 2).map { ($0.key, $0.value) })
        defaults.set(try? JSONEncoder().encode(memory), forKey: Self.defaultsKey)
    }

    static func serviceKey(_ name: String) -> String { "service:\(name)" }

    /// The PC behind a Bonjour service.
    private func key(service: String, in memory: Memory) -> String {
        memory.services[service] ?? named[service] ?? Self.serviceKey(service)
    }

    /// Whether `service` is the name a PC called `hostName` registers, or
    /// that name with a Bonjour collision suffix.
    static func isOwnService(_ service: String, of hostName: String) -> Bool {
        let own = "EternalMonitor (\(hostName))"
        if service == own { return true }
        guard service.hasPrefix(own + " ("), service.hasSuffix(")") else { return false }
        let number = service.dropFirst(own.count + 2).dropLast()
        return !number.isEmpty && number.allSatisfy { $0.isASCII && $0.isNumber }
    }

    /// Links each unlinked session's PC to its own service while the search
    /// lists exactly one, and to nothing otherwise.
    private func linkNames() {
        var memory = self.memory
        var changed = false
        named.removeAll()
        for (hostName, key) in unlinked {
            let matches = listed.map(\.name).filter { Self.isOwnService($0, of: hostName) }
            guard matches.count == 1, let service = matches.first, memory.services[service] == nil else { continue }
            let target = Self.serviceKey(service)
            guard target == key || memory.pcs[target]?.current == nil else { continue }
            named[service] = key
            guard target != key else { continue }
            if let shown = memory.pcs[target]?.shownAt, var pc = memory.pcs[key] {
                pc.shownAt = max(pc.shownAt ?? shown, shown)
                memory.pcs[key] = pc
                changed = true
            }
            if waiting?.key == target { waiting = nil }
            if let shown = card, shown.key == target { card = Card(key: key, pc: shown.pc, version: shown.version) }
        }
        if changed { save(memory) }
    }

    /// The services the search lists now, each with the version its own
    /// TXT record announced.
    func discovered(_ hosts: [DiscoveredHost]) {
        listed = hosts
        linkNames()
        let memory = self.memory
        for host in hosts {
            let key = key(service: host.name, in: memory)
            switch OldPC.isOld(announced: host.version, hostCaps: nil) {
            case true? where memory.pcs[key]?.current != true:
                queue(Card(key: key, pc: OldPC.pcName(bonjour: host.name), version: host.version), memory: memory)
            case false?:
                drop(key)
            default:
                break
            }
        }
    }

    /// The user tapped a found PC at `address`.
    func connecting(service: String, address: String) {
        tapped = (service, address)
    }

    /// A PC accepted a session. `hostCaps` is from that accepted HELLO_ACK;
    /// `address` is nil over USB.
    ///
    /// The session's service is the one the user tapped at that address,
    /// else the one the search lists there. A service already linked to a PC
    /// with a proof of its own stays with it. Without either, the PC's name
    /// may link its own service for this launch (see the type's note).
    func answered(hostName: String, pairingToken: Data, hostCaps: UInt16, address: String?) {
        var memory = self.memory
        let found = address.flatMap { address in
            tapped.flatMap { $0.address == address ? $0.service : nil }
                ?? listed.first { $0.address == address }?.name
        }
        tapped = nil
        var service = found
        var linkedTo: String?
        if let candidate = service {
            let key = Self.key(hostName: hostName, pairingToken: pairingToken, service: candidate)
            let target = memory.services[candidate] ?? Self.serviceKey(candidate)
            if target == key || memory.pcs[target]?.current == nil {
                linkedTo = target
            } else {
                service = nil
            }
        }
        let key = Self.key(hostName: hostName, pairingToken: pairingToken, service: service)
        var pc = memory.pcs[key] ?? Memory.PC(seenAt: now())
        if let service, let old = linkedTo {
            let serviceKey = Self.serviceKey(service)
            if old != key {
                if let shown = memory.pcs[old]?.shownAt { pc.shownAt = max(pc.shownAt ?? shown, shown) }
                if waiting?.key == old { waiting = nil }
                if let shown = card, shown.key == old { card = Card(key: key, pc: shown.pc, version: shown.version) }
                // Only a record without a proof of its own goes.
                if old == serviceKey { memory.pcs[old] = nil }
            }
            memory.services[service] = key == serviceKey ? nil : key
        }
        let old = OldPC.isOld(announced: nil, hostCaps: hostCaps) == true
        pc.current = !old
        pc.seenAt = now()
        memory.pcs[key] = pc
        save(memory)
        // A PC answers under one name at a time.
        unlinked = unlinked.filter { $0.value != key }
        if found == nil, !hostName.isEmpty { unlinked[hostName] = key }
        linkNames()
        if old {
            queue(Card(key: key, pc: hostName, version: nil), memory: self.memory)
        } else {
            drop(key)
        }
    }

    /// The pairing token's hash when the PC has one, otherwise the service
    /// it was found as, otherwise its name.
    static func key(hostName: String, pairingToken: Data, service: String?) -> String {
        if PairingStore.valid(pairingToken) {
            return "pair:" + SHA256.hash(data: pairingToken).prefix(12).map { String(format: "%02x", $0) }.joined()
        }
        if let service { return serviceKey(service) }
        return "name:\(hostName)"
    }

    private func queue(_ next: Card, memory: Memory) {
        guard card == nil, due(next.key, in: memory) else { return }
        waiting = next
    }

    private func drop(_ key: String) {
        if waiting?.key == key { waiting = nil }
        if card?.key == key { card = nil }
    }

    /// The Connect screen is in view with nothing streaming: shows the
    /// waiting card, if its PC is due.
    func show() {
        guard card == nil, let next = waiting else { return }
        waiting = nil
        var memory = self.memory
        guard due(next.key, in: memory) else { return }
        var pc = memory.pcs[next.key] ?? Memory.PC(seenAt: now())
        pc.shownAt = now()
        pc.seenAt = now()
        memory.pcs[next.key] = pc
        save(memory)
        card = next
        log("old_pc_prompt shown=true version=\(next.version ?? "unknown")")
    }

    /// Closed, or the Connect screen gave way to a stream.
    func close() {
        card = nil
    }

    /// Whether the PC `key` may get its card again. A showing dated after
    /// now, from a clock that was ahead, doesn't hold it back.
    func due(_ key: String) -> Bool { due(key, in: memory) }

    private func due(_ key: String, in memory: Memory) -> Bool {
        guard let last = memory.pcs[key]?.shownAt else { return true }
        let now = now()
        return last > now || now.timeIntervalSince(last) >= Self.interval
    }

    #if DEBUG
    func preview(_ card: Card) {
        self.card = card
    }
    #endif
}
