import XCTest
@testable import EternalMonitor

/// Drives the real reassembler and reads its counters at each delivered frame,
/// the way ConnectionManager does, to show which deliveries follow a loss.
final class ReferenceLossDetectorTests: XCTestCase {
    private var now: UInt64 = 0
    private var assembler = FrameAssembler()
    private var timeline: [String] = []

    private func setUp(repair: Bool) {
        now = 0
        timeline = []
        assembler = FrameAssembler(nowUs: { [weak self] in self?.now ?? 0 })
        assembler.repairEnabled = repair
        var detector = ReferenceLossDetector()
        assembler.onFrameAssembled = { [weak self] _, seq, _, _ in
            guard let self else { return }
            if detector.lostSincePreviousFrame(self.assembler.counters.withLock { $0 }) { self.timeline.append("lost") }
            self.timeline.append("frame \(seq)")
        }
    }

    private func add(_ seq: UInt32, index: UInt16 = 0, count: UInt16 = 1, epoch: UInt32 = 1) {
        assembler.addFragment(seq: seq, index: index, count: count, epoch: epoch, isKeyframe: false,
                              captureTimestampUs: 0, payload: Data([UInt8(truncatingIfNeeded: seq)]))
    }

    func testFramesInOrderAreNoLoss() {
        setUp(repair: true)
        for seq in UInt32(1)...5 { add(seq) }
        XCTAssertEqual(timeline, (1...5).map { "frame \($0)" })
    }

    func testFrameDroppedAtItsRepairDeadlineMarksTheNextFrame() {
        setUp(repair: true)
        add(1)
        add(2, index: 1, count: 2)
        now = 2_000
        add(3)
        // Past the repair deadline and the most a stalled repair path adds.
        assembler.tick(at: 200_000)
        add(4)
        XCTAssertEqual(timeline, ["frame 1", "lost", "frame 3", "frame 4"])
    }

    func testFrameDroppedForCapacityMarksTheNextFrame() {
        setUp(repair: true)
        add(1)
        add(2, count: 2)
        for seq in UInt32(3)...UInt32(FrameAssembler.maxPendingFrames + 2) { add(seq) }
        XCTAssertEqual(Array(timeline.prefix(4)), ["frame 1", "lost", "frame 3", "frame 4"])
    }

    func testStreamRestartIsALoss() {
        setUp(repair: false)
        add(7, epoch: 1)
        add(8, epoch: 1)
        add(1, epoch: 2)
        XCTAssertEqual(timeline, ["frame 7", "frame 8", "lost", "frame 1"])
    }

    func testOldHostRestartWithoutAnEpochIsALoss() {
        // Hosts before stream epochs restart with a large backward jump in seq.
        setUp(repair: false)
        add(900, epoch: 0)
        add(901, epoch: 0)
        add(3, epoch: 0)
        XCTAssertEqual(timeline, ["frame 900", "frame 901", "lost", "frame 3"])
    }

    func testDropWithoutRepairIsSeenByTheFrameAfterIt() {
        // Without repair (USB, older hosts) the reassembler delivers the newer
        // frame before counting the older partial one as dropped, so the loss
        // shows one frame late. Reported to the stream-smoothness owner.
        setUp(repair: false)
        add(1)
        add(2, count: 2)
        add(3)
        add(4)
        XCTAssertEqual(timeline, ["frame 1", "frame 3", "lost", "frame 4"])
    }
}
