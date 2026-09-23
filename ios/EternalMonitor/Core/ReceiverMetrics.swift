import Foundation
import os

/// Decode callbacks and receiver reports run on different queues. Keep the
/// measurements here so reports do not depend on a delayed UI refresh.
final class ReceiverMetrics {
    private struct Samples {
        var decodedAt: [UInt64] = []
        var lastDecodeUs: UInt64 = 0
        var lastCaptureUs: UInt64 = 0
    }
    private let samples = OSAllocatedUnfairLock(initialState: Samples())

    func recordDecoded(at now: UInt64, captureUs: UInt64) {
        samples.withLock {
            $0.decodedAt.append(now)
            $0.decodedAt.removeAll { now > $0 && now - $0 > 1_000_000 }
            $0.lastDecodeUs = now
            $0.lastCaptureUs = captureUs
        }
    }

    func makeReport(assembler: FrameAssembler.Counters, decodeDepth: UInt8,
                    clock: ControlChannel.ClockSnapshot, now: UInt64) -> ReceiverReport {
        var report = ReceiverReport(
            streamEpoch: assembler.streamEpoch, highestSeq: assembler.highestSeq,
            framesComplete: UInt32(truncatingIfNeeded: assembler.framesComplete),
            framesDropped: UInt32(truncatingIfNeeded: assembler.framesDropped),
            fragsReceived: UInt32(truncatingIfNeeded: assembler.fragsReceived),
            fragsLost: UInt32(truncatingIfNeeded: assembler.fragsLost), jitterUs: assembler.jitterUs,
            assemblerDepth: assembler.assemblerDepth, decodeDepth: decodeDepth,
            rttMsX10: UInt16(clamping: (clock.rttUs ?? 0) / 100),
            fragsRepaired: UInt32(truncatingIfNeeded: assembler.fragsRepaired),
            nacksSent: UInt32(truncatingIfNeeded: assembler.nacksSent)
        )
        let decoded = samples.withLock { state -> (UInt16, UInt16) in
            var fps: UInt16 = 0
            var latencyX10: UInt16 = 0
            state.decodedAt.removeAll { now > $0 && now - $0 > 1_000_000 }
            if state.decodedAt.count >= 2, let first = state.decodedAt.first,
               let last = state.decodedAt.last, last > first {
                fps = UInt16(clamping: UInt64(state.decodedAt.count - 1) * 10_000_000 / (last - first))
            }
            if let offset = clock.offsetUs, state.lastCaptureUs > 0 {
                let latency = Double(state.lastDecodeUs) - (Double(state.lastCaptureUs) - Double(offset))
                latencyX10 = UInt16(min(Double(UInt16.max), max(0, latency) / 100))
            }
            return (fps, latencyX10)
        }
        report.decodeFpsX10 = decoded.0
        report.e2eLatencyMsX10 = decoded.1
        return report
    }
}
