import XCTest
import os
@testable import EternalMonitor

final class KeyframeRequestTests: XCTestCase {
    /// A stall that damages many frames at once must ask the host once, as the
    /// host grants only one keyframe per 500 ms.
    func testRequestsWithinTheHostWindowAreCoalesced() async throws {
        let requests = try await sent([(1_000_000, KeyframeReason.gapLoss), (1_010_000, .gapLoss),
                                       (1_400_000, .decodeError), (1_500_000, .gapLoss)])
        XCTAssertEqual(requests, [.gapLoss, .gapLoss], "one request per 500 ms window")
    }

    /// No first frame yet: the Startup request goes out although dropped
    /// frames asked moments ago, so the PC's log shows it.
    func testAStartupRequestIsNeverCoalesced() async throws {
        let requests = try await sent([(1_000_000, KeyframeReason.gapLoss), (1_100_000, .startup),
                                       (1_200_000, .gapLoss), (1_700_000, .gapLoss)])
        XCTAssertEqual(requests, [.gapLoss, .startup, .gapLoss])
    }

    private let lossAfterKeyframes: [(UInt64, Event)] = [
        (1_000_000, .request(.gapLoss)), (1_050_000, .keyframe),
        (1_100_000, .request(.gapLoss)),
        (1_120_000, .request(.gapLoss)), // the same loss
        (1_300_000, .request(.decodeError)),
        (1_500_000, .request(.gapLoss)), // 500 ms after the first
        (1_600_000, .keyframe),
        (1_650_000, .request(.decodeError)),
        (1_700_000, .request(.gapLoss)),
    ]

    /// A frame lost after a keyframe arrived is a new loss: to a PC that
    /// answers those, its GapLoss goes at once, once per keyframe, and leaves
    /// the 500 ms spacing as it was. The decoder's requests name no frame,
    /// so they keep the spacing.
    func testALossAfterAKeyframeAsksAtOnce() async throws {
        let requests = try await sent(lossAfterKeyframes, hostCaps: HelloAck.hostCapKeyframePerLoss)
        XCTAssertEqual(requests.map(\.at), [1_000_000, 1_100_000, 1_500_000, 1_700_000])
        XCTAssertTrue(requests.allSatisfy { $0.reason == .gapLoss })
    }

    /// An older PC moves its window for any grant, so it gets one request
    /// per 500 ms, as before.
    func testAnOlderPCIsNotAskedEarly() async throws {
        let requests = try await sent(lossAfterKeyframes, hostCaps: 0)
        XCTAssertEqual(requests.map(\.at), [1_000_000, 1_500_000])
    }

    private enum Event {
        case request(KeyframeReason)
        /// The assembler completed a keyframe.
        case keyframe
    }

    /// The keyframe requests a session's channel sends for `requests`, each
    /// made at its time on the channel's clock.
    private func sent(_ requests: [(UInt64, KeyframeReason)]) async throws -> [KeyframeReason] {
        try await sent(requests.map { ($0.0, Event.request($0.1)) }).map(\.reason)
    }

    /// The keyframe requests a session's channel sends for `events`, each at
    /// its time on the channel's clock, with the time each was sent.
    private func sent(_ events: [(UInt64, Event)], hostCaps: UInt16 = 0) async throws
        -> [(at: UInt64, reason: KeyframeReason)] {
        let queue = DispatchQueue(label: "keyframe-request-test")
        let clock = OSAllocatedUnfairLock<UInt64>(initialState: 1_000_000)
        let (stream, continuation) = AsyncStream<Data>.makeStream()
        let sentAt = OSAllocatedUnfairLock<[UInt64]>(initialState: [])
        let channel = ControlChannel(queue: queue, nowUs: { clock.withLock { $0 } }) { data in
            if case .keyframeRequest = Wire.parseControl(data)?.message {
                sentAt.withLock { $0.append(clock.withLock { $0 }) }
            }
            continuation.yield(data)
        }
        defer { channel.stop(); continuation.finish() }
        channel.startHandshake(listenPort: 50000, identity: ControlChannel.ClientIdentity(
            deviceName: "Test iPad", screenPxW: 640, screenPxH: 360, screenPtW: 640, screenPtH: 360, refreshHz: 60))
        var messages = stream.makeAsyncIterator()
        let first = await messages.next()
        guard case .hello2(let hello) = try XCTUnwrap(Wire.parseControl(XCTUnwrap(first))).message else {
            XCTFail("missing handshake")
            return []
        }
        queue.sync {
            channel.handleControl(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .helloAck(HelloAck(
                status: .ok, acceptedVersion: 2, clientNonce: hello.clientNonce, sessionId: 41,
                heartbeatIntervalMs: 1000, reportIntervalMs: 500, livenessTimeoutMs: 3000,
                streamConfig: StreamConfig(), hostName: "Test PC", authToken: Data(17...32), hostCaps: hostCaps))))
        }
        for (at, event) in events {
            clock.withLock { $0 = at }
            switch event {
            case .request(let reason):
                channel.sendKeyframeRequest(streamEpoch: 1, lastCompleteSeq: 7, reason: reason)
            case .keyframe:
                channel.noteKeyframeAssembled()
            }
            queue.sync {}
        }
        channel.stop()
        continuation.finish()
        var reasons: [KeyframeReason] = []
        while let data = await messages.next() {
            if case .keyframeRequest(let request) = Wire.parseControl(data)?.message { reasons.append(request.reason) }
        }
        return Array(zip(sentAt.withLock { $0 }, reasons))
    }
}
