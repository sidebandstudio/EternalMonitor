import XCTest
@testable import EternalMonitor

final class ReceiverMetricsTests: XCTestCase {
    func testReportsAssemblerDecoderAndClockMeasurements() {
        let metrics = ReceiverMetrics()
        for frame in 0...60 {
            let now = 1_000_000 + UInt64(frame) * 1_000_000 / 60
            metrics.recordDecoded(at: now, captureUs: now + 40_000)
        }
        let counts = FrameAssembler.Counters(framesComplete: 600, framesDropped: 2,
            fragsReceived: 3000, fragsLost: 3, fragsRepaired: 40, nacksSent: 35,
            highestSeq: 602, streamEpoch: 7, assemblerDepth: 2, jitterUs: 1234)
        let report = metrics.makeReport(assembler: counts, decodeDepth: 3,
            clock: ControlChannel.ClockSnapshot(offsetUs: 50_000, rttUs: 2800), now: 2_000_000)
        XCTAssertEqual(report, ReceiverReport(streamEpoch: 7, highestSeq: 602,
            framesComplete: 600, framesDropped: 2, fragsReceived: 3000, fragsLost: 3,
            jitterUs: 1234, decodeFpsX10: 600, assemblerDepth: 2, decodeDepth: 3,
            e2eLatencyMsX10: 100, rttMsX10: 28, fragsRepaired: 40, nacksSent: 35))
    }

    func testLatencyWaitsForClockSyncAndFPSExpires() {
        let metrics = ReceiverMetrics()
        metrics.recordDecoded(at: 1_000_000, captureUs: 1)
        metrics.recordDecoded(at: 1_016_667, captureUs: 2)
        let report = metrics.makeReport(assembler: .init(), decodeDepth: 0, clock: .init(), now: 3_000_000)
        XCTAssertEqual(report.decodeFpsX10, 0)
        XCTAssertEqual(report.e2eLatencyMsX10, 0)
        XCTAssertEqual(report.audioPacketsLost, 0)
        XCTAssertEqual(report.audioBufferMs, 0)
    }
}
