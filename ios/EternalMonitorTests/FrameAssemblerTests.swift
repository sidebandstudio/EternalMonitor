import XCTest
@testable import EternalMonitor

final class FrameAssemblerTests: XCTestCase {
    private var assembler = FrameAssembler()
    private var completed: [Data] = []
    private var diagnostics: [String] = []
    private var now: UInt64 = 0
    private var nacks: [Nack] = []
    private var keyframes = 0

    override func setUp() {
        super.setUp()
        assembler = FrameAssembler()
        completed = []
        diagnostics = []
        assembler.onFrameAssembled = { [weak self] data, _, _, _ in self?.completed.append(data) }
        assembler.onDiagnostic = { [weak self] message in self?.diagnostics.append(message) }
    }

    private func add(seq: UInt32, index: UInt16, count: UInt16, epoch: UInt32 = 1, byte: UInt8, retransmit: Bool = false, keyframe: Bool = false) {
        assembler.addFragment(
            seq: seq, index: index, count: count, epoch: epoch,
            isKeyframe: keyframe, captureTimestampUs: 0, payload: Data([byte]), isRetransmit: retransmit
        )
    }

    private func enableRepair() {
        now = 0
        nacks = []
        keyframes = 0
        assembler = FrameAssembler(nowUs: { [weak self] in self?.now ?? 0 })
        assembler.repairEnabled = true
        assembler.onFrameAssembled = { [weak self] data, _, _, _ in self?.completed.append(data) }
        assembler.onNack = { [weak self] nack in self?.nacks.append(nack) }
        assembler.onNeedsKeyframe = { [weak self] _, _ in self?.keyframes += 1 }
    }

    func testSharedRustSwiftRepairTraces() throws {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "repair_vectors", withExtension: "txt", subdirectory: "testdata"))
        let text = try String(contentsOf: url, encoding: .utf8)
        var delivered: [UInt32] = []
        for (line, row) in text.components(separatedBy: .newlines).enumerated() {
            let fields = row.split(whereSeparator: { $0.isWhitespace }).map(String.init)
            guard let command = fields.first, !command.hasPrefix("#") else { continue }
            func number(_ index: Int) -> UInt64 { UInt64(fields[index])! }
            switch command {
            case "reset":
                enableRepair()
                assembler.framePeriodUs = number(1)
                let rtt = number(2)
                assembler.rttUs = { rtt }
                delivered = []
                assembler.onFrameAssembled = { _, seq, _, _ in delivered.append(seq) }
            case "add":
                now = number(1)
                add(seq: UInt32(number(3)), index: UInt16(number(4)), count: UInt16(number(5)),
                    epoch: UInt32(number(2)), byte: UInt8(number(7)), retransmit: number(6) != 0,
                    keyframe: fields.count > 8 && number(8) != 0)
            case "tick":
                assembler.tick(at: number(1))
            case "expect":
                let c = assembler.counters.withLock { $0 }
                XCTAssertEqual([c.framesComplete, c.framesDropped, c.fragsReceived, c.fragsLost,
                    c.fragsRepaired, c.nacksSent, UInt64(c.assemblerDepth)], (1...7).map(number), "line \(line + 1)")
                XCTAssertEqual(delivered.isEmpty ? "-" : delivered.map(String.init).joined(separator: ","), fields[8], "line \(line + 1)")
                XCTAssertEqual(UInt64(keyframes), number(9), "line \(line + 1)")
                let requests = nacks.map { "\($0.frameSeq):" + $0.missing.map(String.init).joined(separator: ",") }
                XCTAssertEqual(requests.isEmpty ? "-" : requests.joined(separator: "|"), fields[10], "line \(line + 1)")
            default: XCTFail("Unknown trace command \(command)")
            }
        }
    }

    func testFirstGapRequestsOnlyProvenMissingIndices() {
        enableRepair()
        add(seq: 1, index: 2, count: 100, byte: 3)
        XCTAssertEqual(nacks, [Nack(streamEpoch: 1, frameSeq: 1, fragCount: 100, missing: [0, 1])])
        add(seq: 1, index: 2, count: 100, byte: 3)
        XCTAssertEqual(nacks.count, 1)
        XCTAssertEqual(assembler.counters.withLock { $0.fragsReceived }, 1)
    }

    func testRetransmitCompletesHeldFramesInOrder() {
        enableRepair()
        add(seq: 1, index: 0, count: 2, byte: 1)
        now = 10_000
        add(seq: 2, index: 0, count: 1, byte: 3)
        XCTAssertTrue(completed.isEmpty)
        XCTAssertEqual(nacks.first?.missing, [1])
        now = 12_000
        add(seq: 1, index: 1, count: 2, byte: 2, retransmit: true)
        XCTAssertEqual(completed, [Data([1, 2]), Data([3])])
        let counts = assembler.counters.withLock { $0 }
        XCTAssertEqual(counts.framesComplete, 2)
        XCTAssertEqual(counts.framesDropped, 0)
        XCTAssertEqual(counts.fragsRepaired, 1)
        XCTAssertEqual(counts.fragsReceived, 2)
        XCTAssertEqual(counts.assemblerDepth, 0)
    }

    /// While repairs keep arriving for other frames, a frame whose own
    /// replacement never comes expires at its deadline.
    func testDeadlineDropsMissingFrameAndReleasesQueue() {
        enableRepair()
        add(seq: 1, index: 1, count: 2, byte: 1)
        now = 2_000
        add(seq: 2, index: 1, count: 2, byte: 2)
        now = 4_000
        add(seq: 2, index: 0, count: 2, byte: 3, retransmit: true)
        now = 12_000
        add(seq: 3, index: 1, count: 2, byte: 4)
        now = 14_000
        add(seq: 3, index: 0, count: 2, byte: 5, retransmit: true)
        assembler.tick(at: 24_999)
        XCTAssertTrue(completed.isEmpty)
        assembler.tick(at: 25_000)
        XCTAssertEqual(completed, [Data([3, 2]), Data([5, 4])])
        XCTAssertEqual(keyframes, 1)
        now = 26_000
        add(seq: 1, index: 0, count: 2, byte: 6, retransmit: true)
        XCTAssertEqual(completed.count, 2)
        let counts = assembler.counters.withLock { $0 }
        XCTAssertEqual(counts.framesDropped, 1)
        XCTAssertEqual(counts.fragsLost, 1)
        XCTAssertEqual(counts.fragsRepaired, 2)
    }

    /// WiFi can hold the uplink for about 100 ms. With no repair arriving for
    /// any frame, the deadline waits and a late replacement still completes
    /// its frame; the wait is bounded at 100 ms past the deadline.
    func testSilentRepairPathDelaysTheDeadlineWithinItsAllowance() {
        enableRepair()
        add(seq: 1, index: 1, count: 2, byte: 2)
        now = 1_000
        add(seq: 2, index: 0, count: 1, byte: 3)
        assembler.tick(at: 60_000)
        XCTAssertTrue(completed.isEmpty)
        now = 61_000
        add(seq: 1, index: 0, count: 2, byte: 1, retransmit: true)
        XCTAssertEqual(completed, [Data([1, 2]), Data([3])])
        XCTAssertEqual(keyframes, 0)

        enableRepair()
        add(seq: 1, index: 1, count: 2, byte: 2)
        assembler.tick(at: 124_999)
        XCTAssertEqual(keyframes, 0)
        assembler.tick(at: 125_000)
        XCTAssertEqual(keyframes, 1)
    }

    func testNackRetriesOnceAfterRTTPlusFiveMilliseconds() {
        enableRepair()
        add(seq: 1, index: 1, count: 2, byte: 2)
        assembler.tick(at: 14_999)
        XCTAssertEqual(nacks.count, 1)
        assembler.tick(at: 15_000)
        XCTAssertEqual(nacks.count, 2)
        XCTAssertEqual(nacks[1].missing, [0])
        assembler.tick(at: 20_000)
        XCTAssertEqual(nacks.count, 2)
        assembler.tick(at: 60_000)
        XCTAssertEqual(nacks.count, 2, "one retry, even while the repair path is silent")
        assembler.tick(at: 125_000)
        XCTAssertEqual(keyframes, 1)
    }

    func testMissingTailIsDetectedWithoutAnotherDatagram() {
        enableRepair()
        add(seq: 1, index: 0, count: 2, byte: 1)
        assembler.tick(at: 16_666)
        XCTAssertTrue(nacks.isEmpty)
        assembler.tick(at: 16_667)
        XCTAssertEqual(nacks.first?.missing, [1])
        // Nothing else arrives, so the frame uses its stall allowance.
        assembler.tick(at: 141_666)
        XCTAssertEqual(keyframes, 0)
        assembler.tick(at: 141_667)
        XCTAssertEqual(keyframes, 1)
        XCTAssertEqual(assembler.counters.withLock { $0.framesDropped }, 1)
    }

    func testRepairHoldsAtMostEightFrames() {
        enableRepair()
        add(seq: 1, index: 0, count: 2, byte: 1)
        for seq in UInt32(2)...8 { add(seq: seq, index: 0, count: 1, byte: UInt8(seq)) }
        XCTAssertTrue(completed.isEmpty)
        add(seq: 9, index: 0, count: 1, byte: 9)
        XCTAssertEqual(completed, (2...9).map { Data([UInt8($0)]) })
        XCTAssertEqual(assembler.counters.withLock { $0.framesDropped }, 1)
        XCTAssertEqual(keyframes, 1)
    }

    /// Measured on a physical iPad over WiFi: fragments arrive a few
    /// milliseconds out of order, and a stall releases several frames at once.
    func testLateFragmentCompletesItsFrameAfterABurstOfNewerFrames() {
        enableRepair()
        add(seq: 1, index: 0, count: 2, byte: 1)
        now = 1_000
        for seq in UInt32(2)...4 { add(seq: seq, index: 0, count: 1, byte: UInt8(seq)) }
        XCTAssertTrue(completed.isEmpty)
        now = 3_000
        add(seq: 1, index: 1, count: 2, byte: 5)
        XCTAssertEqual(completed, [Data([1, 5]), Data([2]), Data([3]), Data([4])])
        XCTAssertEqual(assembler.counters.withLock { $0.framesDropped }, 0)
        XCTAssertEqual(keyframes, 0)
    }

    func testWideGapIsRequestedInNacksOfAtMostSixtyFour() {
        enableRepair()
        add(seq: 1, index: 65, count: 66, byte: 1)
        XCTAssertEqual(nacks.map(\.missing), [Array(0..<64), [64]])
        XCTAssertTrue(nacks.allSatisfy(\.isValid))
        XCTAssertEqual(keyframes, 0)
        assembler.tick(at: 125_000)
        XCTAssertEqual(keyframes, 1)
        XCTAssertEqual(assembler.counters.withLock { $0.fragsLost }, 65)
    }

    func testLateFragmentsCompleteAWideGapWithoutAKeyframe() {
        // A 40 Mbps frame is 72 fragments; WiFi held all but the first for
        // longer than a frame period, then delivered them.
        enableRepair()
        add(seq: 1, index: 0, count: 72, byte: 0)
        now = 17_000
        assembler.tick()
        XCTAssertEqual(nacks.map(\.missing), [Array(1...64), Array(65...71)])
        now = 20_000
        for index in UInt16(1)...71 { add(seq: 1, index: index, count: 72, byte: UInt8(index)) }
        XCTAssertEqual(completed.count, 1)
        XCTAssertEqual(keyframes, 0)
        XCTAssertEqual(assembler.counters.withLock { $0.framesDropped }, 0)
    }

    func testRepairDeadlineUsesRTTAndFramePeriodWithEightMillisecondFloor() {
        enableRepair()
        assembler.framePeriodUs = 1_000
        assembler.rttUs = { 100 }
        // Media and repairs keep flowing, so no stall stretches the deadline.
        add(seq: 1, index: 1, count: 2, byte: 1)
        for (at, seq, index, retransmit) in [
            (1_000, 2, 1, false), (2_000, 2, 0, true), (3_000, 3, 0, false), (4_000, 4, 0, false),
            (5_000, 5, 1, false), (6_000, 5, 0, true), (7_000, 6, 0, false),
        ] as [(UInt64, UInt32, UInt16, Bool)] {
            now = at
            let count: UInt16 = [2, 5].contains(seq) ? 2 : 1
            add(seq: seq, index: index, count: count, byte: UInt8(seq), retransmit: retransmit)
        }
        assembler.tick(at: 7_999)
        XCTAssertEqual(keyframes, 0)
        assembler.tick(at: 8_000)
        XCTAssertEqual(keyframes, 1)
    }

    func testJitterExcludesRetransmitsAndReportsActualDepth() {
        enableRepair()
        now = 100_000
        add(seq: 1, index: 0, count: 3, byte: 1)
        now = 101_000
        add(seq: 1, index: 1, count: 3, byte: 2)
        XCTAssertEqual(assembler.counters.withLock { $0.jitterUs }, 62)
        XCTAssertEqual(assembler.counters.withLock { $0.assemblerDepth }, 1)
        now = 102_000
        add(seq: 1, index: 2, count: 3, byte: 3, retransmit: true)
        XCTAssertEqual(assembler.counters.withLock { $0.jitterUs }, 62)
        XCTAssertEqual(assembler.counters.withLock { $0.assemblerDepth }, 0)
    }

    func testAssemblesInOrderAndOutOfOrder() {
        add(seq: 1, index: 0, count: 3, byte: 0xA)
        add(seq: 1, index: 2, count: 3, byte: 0xC)
        add(seq: 1, index: 1, count: 3, byte: 0xB)
        XCTAssertEqual(completed, [Data([0xA, 0xB, 0xC])])

        add(seq: 2, index: 0, count: 1, byte: 0xD)
        XCTAssertEqual(completed.last, Data([0xD]))
    }

    func testDuplicateOfCompletedFrameIsIgnored() {
        add(seq: 1, index: 0, count: 1, byte: 0xA)
        add(seq: 1, index: 0, count: 1, byte: 0xA)
        XCTAssertEqual(completed.count, 1)
    }

    func testStaleSeqDroppedButBigBackjumpResets() {
        add(seq: 500, index: 0, count: 1, byte: 0xA)
        add(seq: 499, index: 0, count: 1, byte: 0xB)
        XCTAssertEqual(completed.count, 1, "stale frame within the gap must be dropped")

        add(seq: 1, index: 0, count: 1, byte: 0xC)
        XCTAssertEqual(completed.count, 2, "seq back-jump beyond the gap is a stream restart")
        XCTAssertEqual(completed.last, Data([0xC]))
    }

    func testHigherEpochResetsLowerEpochDropped() {
        add(seq: 900, index: 0, count: 1, epoch: 5, byte: 0xA)
        add(seq: 1, index: 0, count: 1, epoch: 6, byte: 0xB)
        XCTAssertEqual(completed.count, 2, "new epoch must reset and accept small seq")

        add(seq: 901, index: 0, count: 1, epoch: 5, byte: 0xC)
        XCTAssertEqual(completed.count, 2, "stale-epoch fragment must be dropped")
    }

    func testAcceptsFramesUpToTheProtocolFragmentCap() {
        // A 1440p/4K scene-change IDR runs past 1024 fragments. The host
        // fragments against the protocol cap and the wire parser accepts up to
        // it, so an assembler cap below that silently dropped every fragment
        // of such a frame — and with no sync sample the decoder then discarded
        // everything after it too.
        XCTAssertEqual(
            FrameAssembler.maxFragmentCount, MediaHeader.maxFragCount,
            "the assembler cap must match the protocol cap the host sends against"
        )

        let count: UInt16 = 2000
        for index in 0..<count {
            add(seq: 1, index: index, count: count, byte: 0xA)
        }
        XCTAssertEqual(completed.count, 1, "a large but legal frame must assemble")
        XCTAssertEqual(completed.first?.count, Int(count))
    }

    func testOneBogusEpochCannotStrandTheStream() {
        add(seq: 10, index: 0, count: 1, epoch: 5, byte: 0xA)
        XCTAssertEqual(completed.count, 1)

        // One corrupted or spoofed fragment claiming the maximum epoch.
        add(seq: 11, index: 0, count: 1, epoch: .max, byte: 0xFF)
        let afterPoison = completed.count

        // The real stream is now "stale" against an epoch the host can never
        // reach. It must not stay that way for the rest of the session.
        for i in 0..<UInt32(600) {
            add(seq: 100 + i, index: 0, count: 1, epoch: 5, byte: 0xB)
        }
        XCTAssertGreaterThan(
            completed.count, afterPoison,
            "the assembler must re-sync to the live stream instead of freezing forever"
        )

        // And it keeps flowing afterwards.
        let beforeTail = completed.count
        add(seq: 900, index: 0, count: 1, epoch: 5, byte: 0xC)
        XCTAssertEqual(completed.count, beforeTail + 1)
        XCTAssertEqual(completed.last, Data([0xC]))
    }

    func testInterleavedStragglersNeverTripTheResync() {
        add(seq: 1, index: 0, count: 1, epoch: 7, byte: 0xA)

        // A real restart: epoch 8 is live while epoch-7 stragglers keep
        // arriving. Far more stale drops than the resync threshold, but the
        // accepted fragments in between must keep resetting the streak.
        for i in 0..<UInt32(1200) {
            add(seq: 1000 + i, index: 0, count: 1, epoch: 8, byte: 0xB)
            add(seq: 500 + i, index: 0, count: 1, epoch: 7, byte: 0xC)
        }
        XCTAssertFalse(
            completed.contains(Data([0xC])),
            "old-run stragglers must never be accepted while the new run is live"
        )
    }

    func testCompletionEvictsOlderPartials() {
        add(seq: 1, index: 0, count: 2, byte: 0xA)
        add(seq: 2, index: 0, count: 1, byte: 0xB)
        XCTAssertEqual(completed, [Data([0xB])])

        // The evicted frame's late fragment is now stale.
        add(seq: 1, index: 1, count: 2, byte: 0xC)
        XCTAssertEqual(completed.count, 1)
    }

    func testMismatchedFragmentCountDoesNotWipeProgress() {
        add(seq: 1, index: 0, count: 3, byte: 0xA)
        add(seq: 1, index: 1, count: 2, byte: 0xB) // conflicting count — ignored
        XCTAssertTrue(diagnostics.contains { $0.contains("mismatched count") })

        add(seq: 1, index: 1, count: 3, byte: 0xB)
        add(seq: 1, index: 2, count: 3, byte: 0xC)
        XCTAssertEqual(completed, [Data([0xA, 0xB, 0xC])], "first-seen count must win")
    }

    func testInvalidFragmentsRejected() {
        add(seq: 1, index: 0, count: 0, byte: 0xA)
        add(seq: 1, index: 2, count: 2, byte: 0xA)
        XCTAssertTrue(completed.isEmpty)
        XCTAssertEqual(diagnostics.count, 2)
    }

    func testFragmentCountAboveCapRejected() {
        add(seq: 1, index: 0, count: FrameAssembler.maxFragmentCount + 1, byte: 0xA)
        XCTAssertTrue(completed.isEmpty)
        XCTAssertTrue(diagnostics.contains { $0.contains("exceeds cap") })
    }

    func testPendingFrameCapDropsOldestFirst() {
        // Fill the pending table with partial frames.
        for seq in 1...UInt32(FrameAssembler.maxPendingFrames) {
            add(seq: seq, index: 0, count: 2, byte: UInt8(seq))
        }
        // One more forces the oldest (seq 1) out.
        add(seq: 100, index: 0, count: 2, byte: 0x64)
        XCTAssertTrue(diagnostics.contains { $0.contains("capacity") })

        // seq 1 can no longer complete…
        add(seq: 1, index: 1, count: 2, byte: 0x01)
        XCTAssertTrue(completed.isEmpty)

        // …but seq 100 still can.
        add(seq: 100, index: 1, count: 2, byte: 0x65)
        XCTAssertEqual(completed, [Data([0x64, 0x65])])
    }

    func testResetClearsEpochAndState() {
        add(seq: 5, index: 0, count: 2, epoch: 9, byte: 0xA)
        assembler.reset()
        add(seq: 1, index: 0, count: 1, epoch: 2, byte: 0xB)
        XCTAssertEqual(completed, [Data([0xB])], "any epoch must be accepted after reset")
    }
}
