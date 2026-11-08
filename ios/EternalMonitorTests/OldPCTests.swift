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
    }

    private func defaults() -> UserDefaults {
        let name = "OldPCTests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: name) }
        return defaults
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
        let old = HelloAck.hostCapNack

        prompt.show()
        XCTAssertNil(prompt.card, "nothing seen yet")
        prompt.saw(pc: "Studio", announced: "0.3.2", hostCaps: nil)
        XCTAssertNil(prompt.card, "waits for the Connect screen")
        prompt.show()
        XCTAssertEqual(prompt.card, OldPCPrompt.Card(pc: "Studio", version: "0.3.2"))
        XCTAssertEqual(lines, ["old_pc_prompt shown=true version=0.3.2"])
        prompt.show()
        XCTAssertEqual(lines.count, 1, "one line per showing")

        // Another old PC waits while a card is up, and isn't kept.
        prompt.saw(pc: "Office", announced: nil, hostCaps: old)
        prompt.close()
        XCTAssertNil(prompt.card)
        prompt.show()
        XCTAssertNil(prompt.card)

        // The same PC again, later that day and in a new launch: no card.
        now = start + 3_600
        prompt = make()
        prompt.saw(pc: "Studio", announced: nil, hostCaps: old)
        prompt.show()
        XCTAssertNil(prompt.card)

        // Another PC has its own three days. Known only by its HELLO_ACK.
        prompt.saw(pc: "Office", announced: nil, hostCaps: old)
        prompt.show()
        XCTAssertEqual(prompt.card, OldPCPrompt.Card(pc: "Office", version: nil))
        XCTAssertEqual(lines.last, "old_pc_prompt shown=true version=unknown")
        prompt.close()

        now = start + OldPCPrompt.interval - 1
        prompt.saw(pc: "Studio", announced: "0.3.2", hostCaps: nil)
        prompt.show()
        XCTAssertNil(prompt.card, "a second short of three days")
        now = start + OldPCPrompt.interval
        prompt.saw(pc: "Studio", announced: "0.3.2", hostCaps: nil)
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "Studio")
        prompt.close()

        // Shown "tomorrow" by a clock that was ahead, then the clock is fixed.
        now = start + 30 * 86_400
        prompt.saw(pc: "Lab", announced: "0.3.3", hostCaps: nil)
        prompt.show()
        prompt.close()
        now = start + 29 * 86_400
        prompt.saw(pc: "Lab", announced: "0.3.3", hostCaps: nil)
        prompt.show()
        XCTAssertEqual(prompt.card?.pc, "Lab")
        XCTAssertEqual(lines.count, 5)
    }

    /// A current PC never gets a card, and its answer takes a waiting or
    /// showing card for that PC away.
    func testACurrentPCTakesItsCardAway() {
        var lines: [String] = []
        let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { lines.append($0) })
        let current = HelloAck.hostCapNack | HelloAck.hostCapContiguousFrameSeq
        prompt.saw(pc: "Studio", announced: "0.3.4", hostCaps: nil)
        prompt.saw(pc: "Office", announced: nil, hostCaps: current)
        prompt.show()
        XCTAssertNil(prompt.card)

        prompt.saw(pc: "Studio", announced: "0.3.3", hostCaps: nil)
        prompt.saw(pc: "Studio", announced: nil, hostCaps: current)
        prompt.show()
        XCTAssertNil(prompt.card, "updated before the card showed")

        prompt.saw(pc: "Office", announced: "0.3.2", hostCaps: nil)
        prompt.show()
        XCTAssertNotNil(prompt.card)
        prompt.saw(pc: "Office", announced: nil, hostCaps: current)
        XCTAssertNil(prompt.card)
        XCTAssertEqual(lines, ["old_pc_prompt shown=true version=0.3.2"])
    }

    /// Only the last showings of a few PCs are kept.
    func testTheShowingsKeptAreBounded() {
        let store = defaults()
        var now = Date(timeIntervalSince1970: 1_000_000)
        let prompt = OldPCPrompt(defaults: store, now: { now }, log: { _ in })
        for index in 0..<(OldPCPrompt.kept + 4) {
            now += 60
            prompt.saw(pc: "PC \(index)", announced: "0.3.2", hostCaps: nil)
            prompt.show()
            prompt.close()
        }
        let kept = store.dictionary(forKey: OldPCPrompt.defaultsKey) ?? [:]
        XCTAssertEqual(kept.count, OldPCPrompt.kept)
        XCTAssertNil(kept["PC 0"])
        XCTAssertNotNil(kept["PC \(OldPCPrompt.kept + 3)"])
    }

    /// A session's HELLO_ACK reaches the prompt: an old PC's card waits for
    /// the Connect screen, a current PC's doesn't.
    func testASessionsAnswerReachesThePrompt() async throws {
        for (caps, old) in [(UInt16(0x000F), true), (UInt16(0x002F), false)] {
            let prompt = OldPCPrompt(defaults: defaults(), now: { Date() }, log: { _ in })
            let manager = ConnectionManager()
            manager.oldPCPrompt = prompt
            addTeardownBlock { await MainActor.run { manager.cancel() } }
            let host = FakeUDPHost()
            manager.connect(host: "127.0.0.1", port: host.port)
            let accepted = await Task.detached { host.acceptSession(hostCaps: caps) }.value
            XCTAssertTrue(accepted, "the iPad never sent HELLO2")
            let deadline = Date().addingTimeInterval(10)
            while manager.hostInfo == nil, Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
            XCTAssertEqual(manager.hostInfo?.hostCaps, caps)
            manager.cancel()
            prompt.show()
            XCTAssertEqual(prompt.card, old ? OldPCPrompt.Card(pc: "Test PC", version: nil) : nil, "caps \(caps)")
        }
    }
}
