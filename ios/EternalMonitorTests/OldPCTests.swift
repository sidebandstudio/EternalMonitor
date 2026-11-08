import Darwin
import XCTest
@testable import EternalMonitor

/// Telling a PC older than 0.3.4 from a current one, and the Connect
/// screen's card that asks to update it.
@MainActor
final class OldPCTests: XCTestCase {
    /// HELLO_ACKs recorded from hosts built at the tags v0.3.2, v0.3.3 and
    /// v0.3.4 (macOS, synthetic capture, pairing off, PC name OLD-PC),
    /// answering this app's HELLO2. 0.3.2 and 0.3.3 answer with host caps
    /// 0x000F, 0.3.4 with 0x002F.
    static let recordedAcks = [
        "0.3.2": "454d0211000039002fe161dd010000000002443322112fe161dde803f401b80b010000000005d0023c000000c0e1e400064f4c442d5043000000000000000000000000000000000f00",
        "0.3.3": "454d0211000039001d64ed60010000000002443322111d64ed60e803f401b80b010000000005d0023c000000c0e1e400064f4c442d5043000000000000000000000000000000000f00",
        "0.3.4": "454d0211000039000c8c4e43010000000002443322110c8c4e43e803f401b80b010000000005d0023c000000c0e1e400064f4c442d5043000000000000000000000000000000002f00",
    ]

    /// The same hosts' Bonjour TXT records, as `dns-sd -L` showed them.
    static func recordedTXT(_ version: String) -> Data {
        NetService.data(fromTXTRecord: ["version": Data(version.utf8), "proto": Data("2".utf8),
                                        "platform": Data("other".utf8)])
    }

    private struct NotAnAck: Error {}

    private func recordedAck(_ version: String) throws -> HelloAck {
        let hex = Array(try XCTUnwrap(Self.recordedAcks[version]))
        let data = Data(stride(from: 0, to: hex.count, by: 2).map { UInt8(String(hex[$0...$0 + 1]), radix: 16)! })
        guard case .helloAck(let ack) = Wire.parseControl(data)?.message else { throw NotAnAck() }
        return ack
    }

    func testRecordedRepliesFromTaggedPCs() throws {
        for (version, old) in [("0.3.2", true), ("0.3.3", true), ("0.3.4", false)] {
            let ack = try recordedAck(version)
            XCTAssertEqual(ack.status, .ok, version)
            XCTAssertEqual(ack.hostName, "OLD-PC", version)
            // Over USB or a typed address: the HELLO_ACK alone.
            XCTAssertEqual(OldPC.isOld(announced: nil, hostCaps: ack.hostCaps), old, version)
            // Found on the network: the TXT record alone, and both.
            let announced = PCVersions.version(fromTXT: Self.recordedTXT(version))
            XCTAssertEqual(announced, version)
            XCTAssertEqual(OldPC.isOld(announced: announced, hostCaps: nil), old, version)
            XCTAssertEqual(OldPC.isOld(announced: announced, hostCaps: ack.hostCaps), old, version)
        }
    }

    /// Before 0.3.4 a PC's optional capabilities (audio, USB, keyboard, pen)
    /// vary with its hardware and settings. None makes it current; the
    /// numbering bit always does.
    func testOnlyTheNumberingBitMarksACurrentPC() {
        for caps in UInt16(0)..<32 {
            XCTAssertEqual(OldPC.isOld(announced: nil, hostCaps: caps), true, "caps \(caps)")
            XCTAssertEqual(OldPC.isOld(announced: nil, hostCaps: caps | HelloAck.hostCapContiguousFrameSeq), false, "caps \(caps)")
        }
        XCTAssertEqual(OldPC.isOld(announced: nil, hostCaps: 0xFFFF), false, "bits a later PC adds")
    }

    /// Never call a current PC old: whatever says current wins, and a beta's
    /// or a missing version decides nothing by itself.
    func testACurrentPCIsNeverOld() {
        let current = HelloAck.hostCapNack | HelloAck.hostCapContiguousFrameSeq
        XCTAssertEqual(OldPC.isOld(announced: "0.3.2", hostCaps: current), false, "another PC's version at a reused address")
        XCTAssertEqual(OldPC.isOld(announced: "0.3.4", hostCaps: HelloAck.hostCapNack), false)
        XCTAssertEqual(OldPC.isOld(announced: "0.3.5", hostCaps: nil), false)
        XCTAssertEqual(OldPC.isOld(announced: "1.0.0", hostCaps: nil), false)
        XCTAssertNil(OldPC.isOld(announced: "0.3.4-beta.20261029.1", hostCaps: nil))
        XCTAssertNil(OldPC.isOld(announced: "0.3.3-beta.20261020.2", hostCaps: nil), "a beta from before the bump")
        XCTAssertNil(OldPC.isOld(announced: "", hostCaps: nil))
        XCTAssertNil(OldPC.isOld(announced: nil, hostCaps: nil))
        XCTAssertEqual(OldPC.isOld(announced: "0.3.3-beta.20261020.2", hostCaps: HelloAck.hostCapNack), true)
    }

    func testThePCsNameFromItsBonjourName() {
        XCTAssertEqual(OldPC.pcName(bonjour: "EternalMonitor (OLD-PC)"), "OLD-PC")
        XCTAssertEqual(OldPC.pcName(bonjour: "Studio"), "Studio")
        XCTAssertEqual(OldPC.pcName(bonjour: "EternalMonitor ()"), "EternalMonitor ()")
        XCTAssertEqual(OldPC.pcName(bonjour: "EternalMonitor (Lab) (2)"), "Lab (2)")
        XCTAssertEqual(OldPC.pcName(bonjour: "EternalMonitor (Lab (Home)) (12)"), "Lab (Home) (12)")
        XCTAssertEqual(OldPC.pcName(bonjour: "EternalMonitor (Lab (Home))"), "Lab (Home)")
    }

    private func defaults() -> UserDefaults {
        let name = "OldPCTests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: name) }
        return defaults
    }

    private let tokenA = Data(1...16), tokenB = Data(17...32), tokenC = Data(33...48)
    private let oldCaps: UInt16 = 0x000F, currentCaps: UInt16 = 0x002F

    private func service(_ pc: String, at address: String?, version: String?) -> DiscoveredHost {
        DiscoveredHost(name: "EternalMonitor (\(pc))", address: address, port: 9876, finding: address == nil, version: version)
    }

    /// At most once per PC every three days, on an injected clock, across
    /// launches; a clock that was ahead doesn't hold it back.
    func testTheCardShowsOncePerPCEveryThreeDays() {
        let store = defaults()
        let start = Date(timeIntervalSince1970: 1_000_000)
        var now = start
        var lines: [String] = []
        let make = { OldPCPrompt(defaults: store, now: { now }, log: { lines.append($0) }) }
        var prompt = make()
        let studio = OldPCPrompt.key(hostName: "Studio", pairingToken: tokenA, service: nil)

        prompt.show()
        XCTAssertNil(prompt.card, "nothing seen yet")
        prompt.answered(hostName: "Studio", pairingToken: tokenA, hostCaps: oldCaps, address: nil)
        XCTAssertNil(prompt.card, "waits for the Connect screen")
        prompt.show()
        XCTAssertEqual(prompt.card, OldPCPrompt.Card(key: studio, pc: "Studio", version: nil))
        XCTAssertEqual(lines, ["old_pc_prompt shown=true version=unknown"])
        prompt.show()
        XCTAssertEqual(lines.count, 1, "one line per showing")

        // Another old PC waits while a card is up, and isn't kept.
        prompt.discovered([service("Office", at: "192.0.2.20", version: "0.3.2")])
        prompt.close()
        XCTAssertNil(prompt.card)
        prompt.show()
        XCTAssertNil(prompt.card)

        // The same PC again, later that day and in a new launch: no card.
        now = start + 3_600
        prompt = make()
        prompt.answered(hostName: "Studio", pairingToken: tokenA, hostCaps: oldCaps, address: nil)
        prompt.show()
        XCTAssertNil(prompt.card)

        // Another PC has its own three days. Known only by its TXT record.
        prompt.discovered([service("Office", at: "192.0.2.20", version: "0.3.2")])
        prompt.show()
        XCTAssertEqual(prompt.card, OldPCPrompt.Card(key: OldPCPrompt.serviceKey("EternalMonitor (Office)"),
                                                     pc: "Office", version: "0.3.2"))
        XCTAssertEqual(lines.last, "old_pc_prompt shown=true version=0.3.2")
        prompt.close()

        now = start + OldPCPrompt.interval - 1
        prompt.answered(hostName: "Studio", pairingToken: tokenA, hostCaps: oldCaps, address: nil)
        prompt.show()
        XCTAssertNil(prompt.card, "a second short of three days")
        now = start + OldPCPrompt.interval
        prompt.answered(hostName: "Studio", pairingToken: tokenA, hostCaps: oldCaps, address: nil)
        prompt.show()
        XCTAssertEqual(prompt.card?.key, studio)
        prompt.close()

        // Shown "tomorrow" by a clock that was ahead, then the clock is fixed.
        now = start + 30 * 86_400
        prompt.discovered([service("Lab", at: "192.0.2.30", version: "0.3.3")])
        prompt.show()
        prompt.close()
        now = start + 29 * 86_400
        prompt.discovered([service("Lab", at: "192.0.2.30", version: "0.3.3")])
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "Lab")
        XCTAssertEqual(lines.count, 5)
    }

    /// r1 P1: a PC whose accepted HELLO_ACK proved it current gets no card
    /// from a Bonjour record that says otherwise afterwards, in this launch
    /// or a later one.
    func testProofThatAPCIsCurrentOutlastsItsSession() {
        let store = defaults()
        var lines: [String] = []
        var prompt = OldPCPrompt(defaults: store, now: { Date() }, log: { lines.append($0) })
        // Tapped in the list: the HELLO_ACK says current, then the TXT 0.3.3.
        prompt.connecting(service: "EternalMonitor (Studio)", address: "192.0.2.10")
        prompt.answered(hostName: "Studio", pairingToken: tokenA, hostCaps: currentCaps, address: "192.0.2.10")
        prompt.discovered([service("Studio", at: "192.0.2.10", version: "0.3.3")])
        prompt.show()
        XCTAssertNil(prompt.card)

        prompt = OldPCPrompt(defaults: store, now: { Date() }, log: { lines.append($0) })
        prompt.discovered([service("Studio", at: "192.0.2.10", version: "0.3.3")])
        prompt.show()
        XCTAssertNil(prompt.card, "a new launch")

        // Typed address, while the search lists that PC there; no pairing.
        prompt.discovered([service("Lab", at: "192.0.2.11", version: nil)])
        prompt.answered(hostName: "Lab", pairingToken: Data(count: 16), hostCaps: currentCaps, address: "192.0.2.11")
        prompt.discovered([service("Lab", at: "192.0.2.11", version: "0.3.2")])
        prompt.show()
        XCTAssertNil(prompt.card)
        XCTAssertEqual(lines, [])

        // Only those PCs: another old one still gets its card.
        prompt.discovered([service("Lab", at: "192.0.2.11", version: "0.3.2"),
                           service("Office", at: "192.0.2.12", version: "0.3.3")])
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "Office")
        XCTAssertEqual(lines, ["old_pc_prompt shown=true version=0.3.3"])
    }

    /// r1 P2: a version stays with the service whose TXT record announced it.
    /// A service first seen over IPv6 keeps its own version when its IPv4
    /// address resolves, and a service at an address another PC used before
    /// never takes that PC's version.
    func testAVersionStaysWithItsOwnService() async throws {
        var addr = sockaddr_in6()
        addr.sin6_len = UInt8(MemoryLayout<sockaddr_in6>.size)
        addr.sin6_family = sa_family_t(AF_INET6)
        inet_pton(AF_INET6, "fe80::1", &addr.sin6_addr)
        let ipv6 = withUnsafeBytes(of: &addr) { Data($0) }

        // An old PC was found at 192.0.2.40 earlier and left.
        PCVersions.record("0.3.2", name: "EternalMonitor (Gone)", address: "192.0.2.40")
        addTeardownBlock { await MainActor.run { PCVersions.forget(name: "EternalMonitor (Gone)", address: "192.0.2.40") } }
        let scanner = NetworkScanner()
        scanner.resolveIPv4 = { _ in "192.0.2.40" }
        let beta = DiscoveredHost.resolved(name: "EternalMonitor (New)", port: 9876, addresses: [ipv6],
                                           version: "0.3.4-beta.20261107.1")
        XCTAssertNil(beta.address)
        scanner.merge(beta)
        scanner.lookUpIPv4(hostName: "new.local.", service: beta)
        let deadline = Date().addingTimeInterval(10)
        while scanner.hosts.first?.address == nil, Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertNotNil(scanner.hosts.first?.address, "the IPv4 lookup didn't answer")
        XCTAssertEqual(scanner.hosts.map(\.address), ["192.0.2.40"])
        XCTAssertEqual(scanner.hosts.first?.version, "0.3.4-beta.20261107.1")

        let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { _ in })
        prompt.discovered(scanner.hosts)
        prompt.show()
        XCTAssertNil(prompt.card, "a beta at a reused address")
        prompt.discovered([service("Quiet", at: "192.0.2.40", version: nil)])
        prompt.show()
        XCTAssertNil(prompt.card, "no TXT version at a reused address")

        // The same path keeps an old version too.
        let old = DiscoveredHost.resolved(name: "EternalMonitor (Old)", port: 9876, addresses: [ipv6], version: "0.3.2")
        scanner.resolveIPv4 = { _ in "192.0.2.41" }
        scanner.merge(old)
        scanner.lookUpIPv4(hostName: "old.local.", service: old)
        let oldDeadline = Date().addingTimeInterval(10)
        while scanner.hosts.first(where: { $0.name == old.name })?.address == nil, Date() < oldDeadline {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        prompt.discovered(scanner.hosts)
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "Old")
        XCTAssertEqual(prompt.card?.version, "0.3.2")
    }

    /// r1 P3: a PC is its pairing, not its name. Two PCs with one name each
    /// get a card; a renamed PC doesn't get another; Bonjour's collision
    /// suffix names a second PC, and a session links it to its pairing.
    func testAPCIsItsPairingNotItsName() {
        let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { _ in })
        prompt.answered(hostName: "Studio", pairingToken: tokenA, hostCaps: oldCaps, address: nil)
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "Studio")
        prompt.close()
        prompt.answered(hostName: "Studio", pairingToken: tokenB, hostCaps: oldCaps, address: nil)
        prompt.show()
        XCTAssertNotNil(prompt.card, "another PC with the same name")
        prompt.close()
        prompt.answered(hostName: "Studio Renamed", pairingToken: tokenA, hostCaps: oldCaps, address: nil)
        prompt.show()
        XCTAssertNil(prompt.card, "the first PC, renamed")

        // Two PCs called Lab on one network: Bonjour lists the second as "Lab (2)".
        let lab = service("Lab", at: "192.0.2.50", version: "0.3.2")
        let lab2 = DiscoveredHost(name: "EternalMonitor (Lab) (2)", address: "192.0.2.51", port: 9876, version: "0.3.2")
        prompt.discovered([lab])
        prompt.show()
        XCTAssertEqual(prompt.card?.key, OldPCPrompt.serviceKey(lab.name))
        prompt.close()
        prompt.discovered([lab, lab2])
        prompt.show()
        XCTAssertEqual(prompt.card?.key, OldPCPrompt.serviceKey(lab2.name), "the second Lab is another PC")
        prompt.close()
        // Connecting to the second links its service to its pairing: no new card.
        prompt.connecting(service: lab2.name, address: "192.0.2.51")
        prompt.answered(hostName: "Lab", pairingToken: tokenC, hostCaps: oldCaps, address: "192.0.2.51")
        prompt.show()
        XCTAssertNil(prompt.card, "shown already, as its service")
        let memory = prompt.memory
        let keyC = OldPCPrompt.key(hostName: "Lab", pairingToken: tokenC, service: nil)
        XCTAssertEqual(memory.services[lab2.name], keyC)
        XCTAssertNotNil(memory.pcs[keyC]?.shownAt)
        XCTAssertNil(memory.pcs[OldPCPrompt.serviceKey(lab2.name)])
        XCTAssertFalse(memory.pcs.keys.contains { $0.contains(tokenC.map { String(format: "%02x", $0) }.joined()) },
                       "the token itself isn't kept")
    }

    /// A current PC never gets a card, and its answer takes a waiting or
    /// showing card for that PC away.
    func testACurrentPCTakesItsCardAway() {
        var lines: [String] = []
        let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { lines.append($0) })
        prompt.discovered([service("Studio", at: "192.0.2.10", version: "0.3.4")])
        prompt.answered(hostName: "Den", pairingToken: tokenC, hostCaps: currentCaps, address: nil)
        prompt.show()
        XCTAssertNil(prompt.card)

        prompt.discovered([service("Studio", at: "192.0.2.10", version: "0.3.3")])
        prompt.discovered([service("Studio", at: "192.0.2.10", version: "0.3.4")])
        prompt.show()
        XCTAssertNil(prompt.card, "updated before the card showed")

        prompt.discovered([service("Office", at: "192.0.2.20", version: "0.3.2")])
        prompt.show()
        XCTAssertNotNil(prompt.card)
        prompt.connecting(service: "EternalMonitor (Office)", address: "192.0.2.20")
        prompt.answered(hostName: "Office", pairingToken: tokenB, hostCaps: currentCaps, address: "192.0.2.20")
        XCTAssertNil(prompt.card)
        XCTAssertEqual(lines, ["old_pc_prompt shown=true version=0.3.2"])
    }

    /// r2: a PC streaming over USB has no address to link, so its session
    /// links the service every PC registers, "EternalMonitor (<its name>)".
    /// The same PC over USB and in the search gets one card, in either
    /// order, and a proof over USB holds against its stale TXT record.
    func testAUSBSessionAndItsOwnServiceAreOnePC() {
        for usbFirst in [true, false] {
            var lines: [String] = []
            let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { lines.append($0) })
            let steps: [() -> Void] = [
                { prompt.answered(hostName: "Studio", pairingToken: self.tokenA, hostCaps: self.oldCaps, address: nil) },
                { prompt.discovered([self.service("Studio", at: "192.0.2.10", version: "0.3.3")]) },
            ]
            for step in usbFirst ? steps : steps.reversed() {
                step()
                prompt.show()
                prompt.close()
            }
            XCTAssertEqual(lines.count, 1, "usbFirst \(usbFirst): \(lines)")
        }

        let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { _ in })
        prompt.answered(hostName: "Studio", pairingToken: tokenA, hostCaps: currentCaps, address: nil)
        prompt.discovered([service("Studio", at: "192.0.2.10", version: "0.3.3")])
        prompt.show()
        XCTAssertNil(prompt.card, "proved current over USB")
    }

    /// r3: a name links a service only when the search lists exactly one
    /// for it. B and A both answer as "Lab"; Bonjour lists B as "Lab (2)".
    /// B proving current over USB never hides A's card, in either order.
    func testANameLinksOnlyAnUnambiguousService() {
        let a = DiscoveredHost(name: "EternalMonitor (Lab)", address: "192.0.2.70", port: 9876, version: "0.3.2")
        let b = DiscoveredHost(name: "EternalMonitor (Lab) (2)", address: "192.0.2.71", port: 9876, version: "0.3.4")
        for usbFirst in [true, false] {
            let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { _ in })
            let steps: [() -> Void] = [
                { prompt.answered(hostName: "Lab", pairingToken: self.tokenB, hostCaps: self.currentCaps, address: nil) },
                { prompt.discovered([a, b]) },
            ]
            for step in usbFirst ? steps : steps.reversed() { step() }
            prompt.show()
            XCTAssertEqual(prompt.card?.key, OldPCPrompt.serviceKey(a.name), "usbFirst \(usbFirst): A's card")
        }

        // With A gone, B's own suffixed service is the only one: it is B's,
        // and B's proof holds against its stale TXT record.
        let staleB = DiscoveredHost(name: b.name, address: b.address, port: 9876, version: "0.3.3")
        for usbFirst in [true, false] {
            let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { _ in })
            let steps: [() -> Void] = [
                { prompt.answered(hostName: "Lab", pairingToken: self.tokenB, hostCaps: self.currentCaps, address: nil) },
                { prompt.discovered([staleB]) },
            ]
            for step in usbFirst ? steps : steps.reversed() { step() }
            prompt.show()
            XCTAssertNil(prompt.card, "usbFirst \(usbFirst): B proved current")
            XCTAssertEqual(prompt.memory.services, [:], "kept for this launch only")
        }

        XCTAssertTrue(OldPCPrompt.isOwnService("EternalMonitor (Lab)", of: "Lab"))
        XCTAssertTrue(OldPCPrompt.isOwnService("EternalMonitor (Lab) (2)", of: "Lab"))
        XCTAssertTrue(OldPCPrompt.isOwnService("EternalMonitor (Lab) (12)", of: "Lab"))
        for other in ["EternalMonitor (Lab2)", "EternalMonitor (Lab) ()", "EternalMonitor (Lab) (x)",
                      "EternalMonitor (Lab) (2) (3)", "EternalMonitor (Lab)(2)", "Lab"] {
            XCTAssertFalse(OldPCPrompt.isOwnService(other, of: "Lab"), other)
        }
    }

    /// r4: one old PC used over USB and seen by the search gets one card
    /// every three days, also in later launches, which have no name link.
    /// The search's record may come before the card or after it.
    func testAPCLinkedByNameGetsOneCardAcrossLaunches() {
        let lab = service("Lab", at: "192.0.2.90", version: "0.3.3")
        for listedFirst in [true, false] {
            let store = defaults()
            let start = Date(timeIntervalSince1970: 1_000_000)
            var now = start
            var lines: [String] = []
            let make = { OldPCPrompt(defaults: store, now: { now }, log: { lines.append($0) }) }
            var prompt = make()
            if listedFirst { prompt.discovered([lab]) }
            prompt.answered(hostName: "Lab", pairingToken: tokenA, hostCaps: oldCaps, address: nil)
            prompt.show()
            XCTAssertEqual(prompt.card?.key, OldPCPrompt.key(hostName: "Lab", pairingToken: tokenA, service: nil))
            prompt.close()
            if !listedFirst { prompt.discovered([lab]) }
            prompt.show()
            XCTAssertNil(prompt.card)

            now = start + 86_400
            prompt = make()
            prompt.discovered([lab])
            prompt.show()
            XCTAssertNil(prompt.card, "listedFirst \(listedFirst): day 1, the search only")

            now = start + OldPCPrompt.interval
            prompt = make()
            prompt.discovered([lab])
            prompt.show()
            XCTAssertEqual(prompt.card?.pc, "Lab", "listedFirst \(listedFirst): day 3")
            XCTAssertEqual(lines.count, 2, "listedFirst \(listedFirst): \(lines)")
            XCTAssertEqual(prompt.memory.services, [:])
        }
    }

    /// Bonjour updates that change nothing write nothing.
    func testUnchangedUpdatesWriteNothing() {
        final class Counting: UserDefaults {
            var writes = 0
            override func set(_ value: Any?, forKey defaultName: String) {
                writes += 1
                super.set(value, forKey: defaultName)
            }
        }
        let name = "OldPCTests-\(UUID().uuidString)"
        let store = Counting(suiteName: name)!
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: name) }
        let prompt = OldPCPrompt(defaults: store, now: { Date() }, log: { _ in })
        let lab = service("Lab", at: "192.0.2.90", version: "0.3.3")
        prompt.discovered([lab])
        prompt.show()
        prompt.close()
        prompt.answered(hostName: "Lab", pairingToken: tokenA, hostCaps: oldCaps, address: nil)
        let writes = store.writes
        for _ in 0..<5 { prompt.discovered([lab]) }
        XCTAssertEqual(store.writes, writes)
    }

    /// r3: a name a PC gave up is free for another PC: after a rename in the
    /// same launch, and in any later launch.
    func testANameIsNotKeptForAnotherPC() {
        let store = defaults()
        var prompt = OldPCPrompt(defaults: store, now: { Date() }, log: { _ in })
        prompt.discovered([service("Lab", at: "192.0.2.80", version: nil)])
        prompt.answered(hostName: "Lab", pairingToken: tokenA, hostCaps: currentCaps, address: nil)
        prompt.discovered([service("Lab", at: "192.0.2.80", version: "0.3.3")])
        prompt.show()
        XCTAssertNil(prompt.card, "linked while A is the only Lab")
        XCTAssertEqual(prompt.memory.services, [:])

        // A is renamed; another PC takes the name "Lab".
        prompt.answered(hostName: "Studio", pairingToken: tokenA, hostCaps: currentCaps, address: nil)
        prompt.discovered([service("Lab", at: "192.0.2.81", version: "0.3.2")])
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "Lab", "same launch, after the rename")
        prompt.close()

        let other = defaults()
        prompt = OldPCPrompt(defaults: other, now: { Date() }, log: { _ in })
        prompt.discovered([service("Lab", at: "192.0.2.80", version: nil)])
        prompt.answered(hostName: "Lab", pairingToken: tokenA, hostCaps: currentCaps, address: nil)
        prompt = OldPCPrompt(defaults: other, now: { Date() }, log: { _ in })
        prompt.discovered([service("Lab", at: "192.0.2.81", version: "0.3.2")])
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "Lab", "a later launch")
    }

    /// r2: a link never takes a service, or a proof, from another PC. Here
    /// the search still lists A at an address B answers at now.
    func testALinkNeverTakesAnotherPCsProof() {
        let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { _ in })
        let a = service("A", at: "192.0.2.60", version: nil)
        prompt.discovered([a])
        prompt.answered(hostName: "A", pairingToken: Data(count: 16), hostCaps: currentCaps, address: "192.0.2.60")
        let proof = OldPCPrompt.serviceKey(a.name)
        XCTAssertEqual(prompt.memory.pcs[proof]?.current, true)

        prompt.answered(hostName: "B", pairingToken: tokenB, hostCaps: oldCaps, address: "192.0.2.60")
        let memory = prompt.memory
        XCTAssertEqual(memory.pcs[proof]?.current, true, "A's proof stays")
        XCTAssertNil(memory.services[a.name], "A's service isn't B's")
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "B", "B is old")
        prompt.close()
        prompt.discovered([service("A", at: "192.0.2.61", version: "0.3.3")])
        prompt.show()
        XCTAssertNil(prompt.card, "A proved current")
    }

    /// Only the last few PCs are kept, with their links.
    func testTheMemoryIsBounded() {
        var now = Date(timeIntervalSince1970: 1_000_000)
        let prompt = OldPCPrompt(defaults: defaults(), now: { now }, log: { _ in })
        for index in 0..<(OldPCPrompt.kept + 4) {
            now += 60
            prompt.connecting(service: "EternalMonitor (PC \(index))", address: "192.0.2.\(index)")
            prompt.answered(hostName: "PC \(index)", pairingToken: Data(repeating: UInt8(index + 1), count: 16),
                            hostCaps: oldCaps, address: "192.0.2.\(index)")
            prompt.show()
            prompt.close()
        }
        let memory = prompt.memory
        XCTAssertEqual(memory.pcs.count, OldPCPrompt.kept)
        XCTAssertEqual(memory.services.count, OldPCPrompt.kept)
        XCTAssertNil(memory.services["EternalMonitor (PC 0)"])
        XCTAssertNotNil(memory.services["EternalMonitor (PC \(OldPCPrompt.kept + 3))"])

        // One PC found under many names keeps a few links.
        for index in 0..<(OldPCPrompt.kept * 3) {
            prompt.connecting(service: "EternalMonitor (Alias \(index))", address: "198.51.100.\(index)")
            prompt.answered(hostName: "Alias", pairingToken: tokenA, hostCaps: oldCaps, address: "198.51.100.\(index)")
        }
        XCTAssertEqual(prompt.memory.services.count, OldPCPrompt.kept * 2)
    }

    /// One session with `host`, answered with `status` and `caps`.
    private func session(_ manager: ConnectionManager, _ host: FakeUDPHost, status: HelloStatus = .ok,
                         caps: UInt16) async throws {
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession(status: status, hostCaps: caps) }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        let deadline = Date().addingTimeInterval(10)
        if status == .ok {
            while manager.hostInfo == nil, Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
            XCTAssertEqual(manager.hostInfo?.hostCaps, caps)
        } else {
            try await Task.sleep(nanoseconds: 300_000_000)
            XCTAssertNil(manager.hostInfo)
        }
        manager.cancel()
    }

    /// A session's accepted HELLO_ACK reaches the prompt: an old PC's card
    /// waits for the Connect screen, a current PC's doesn't.
    func testASessionsAnswerReachesThePrompt() async throws {
        for (caps, old) in [(oldCaps, true), (currentCaps, false)] {
            let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { _ in })
            let manager = ConnectionManager()
            manager.oldPCPrompt = prompt
            addTeardownBlock { await MainActor.run { manager.cancel() } }
            try await session(manager, FakeUDPHost(), caps: caps)
            prompt.show()
            XCTAssertEqual(prompt.card?.pc, old ? "Test PC" : nil, "caps \(caps)")
            XCTAssertEqual(prompt.card?.key.hasPrefix("pair:"), old ? true : nil)
            XCTAssertEqual(prompt.memory.pcs.values.map(\.current), [!old])
        }
    }

    /// Refusals and not-streaming replies may carry no caps at all (the PC
    /// keeps them within the HELLO2's size), so they say nothing about the
    /// PC's age either way.
    func testRefusalsAndNotStreamingRepliesCountForNothing() async throws {
        let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { _ in })
        let manager = ConnectionManager()
        manager.oldPCPrompt = prompt
        addTeardownBlock { await MainActor.run { manager.cancel() } }
        for status in [HelloStatus.stopped, .busy, .unauthorized] {
            try await session(manager, FakeUDPHost(), status: status, caps: 0)
        }
        prompt.show()
        XCTAssertNil(prompt.card)
        XCTAssertEqual(prompt.memory, OldPCPrompt.Memory())

        // The same PC accepting a session is what counts.
        try await session(manager, FakeUDPHost(), caps: oldCaps)
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "Test PC")
    }
}
