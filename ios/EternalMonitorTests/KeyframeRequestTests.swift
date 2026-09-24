import XCTest
import os
@testable import EternalMonitor

final class KeyframeRequestTests: XCTestCase {
    /// A stall that damages many frames at once must ask the host once, as the
    /// host grants only one keyframe per 500 ms.
    func testRequestsWithinTheHostWindowAreCoalesced() async throws {
        let queue = DispatchQueue(label: "keyframe-request-test")
        let clock = OSAllocatedUnfairLock<UInt64>(initialState: 1_000_000)
        let (stream, continuation) = AsyncStream<Data>.makeStream()
        let channel = ControlChannel(queue: queue, nowUs: { clock.withLock { $0 } }) { continuation.yield($0) }
        defer { channel.stop(); continuation.finish() }
        channel.startHandshake(listenPort: 50000, identity: ControlChannel.ClientIdentity(
            deviceName: "Test iPad", screenPxW: 640, screenPxH: 360, screenPtW: 640, screenPtH: 360, refreshHz: 60))
        var messages = stream.makeAsyncIterator()
        let first = await messages.next()
        guard case .hello2(let hello) = try XCTUnwrap(Wire.parseControl(XCTUnwrap(first))).message else {
            return XCTFail("missing handshake")
        }
        queue.sync {
            channel.handleControl(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .helloAck(HelloAck(
                status: .ok, acceptedVersion: 2, clientNonce: hello.clientNonce, sessionId: 41,
                heartbeatIntervalMs: 1000, reportIntervalMs: 500, livenessTimeoutMs: 3000,
                streamConfig: StreamConfig(), hostName: "Test PC", authToken: Data(17...32), hostCaps: 0))))
        }
        for (at, reason) in [(1_000_000, KeyframeReason.gapLoss), (1_010_000, .gapLoss),
                             (1_400_000, .decodeError), (1_500_000, .gapLoss)] as [(UInt64, KeyframeReason)] {
            clock.withLock { $0 = at }
            channel.sendKeyframeRequest(streamEpoch: 1, lastCompleteSeq: 7, reason: reason)
            queue.sync {}
        }
        channel.stop()
        continuation.finish()
        var requests: [KeyframeReason] = []
        while let data = await messages.next() {
            if case .keyframeRequest(let request) = Wire.parseControl(data)?.message { requests.append(request.reason) }
        }
        XCTAssertEqual(requests, [.gapLoss, .gapLoss], "one request per 500 ms window")
    }
}
