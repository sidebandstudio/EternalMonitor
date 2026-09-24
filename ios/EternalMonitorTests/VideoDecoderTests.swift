import XCTest
import CoreVideo
import os
@testable import EternalMonitor

/// Runs the real VideoToolbox path. In the simulator this exercises the
/// software decoder — which is exactly the fallback the Enable- (not Require-)
/// hardware specification exists to provide, and what the automated E2E
/// depends on.
final class VideoDecoderTests: XCTestCase {
    private func fixtureData() throws -> Data {
        let url = try XCTUnwrap(
            Bundle(for: VideoDecoderTests.self).url(
                forResource: "single_idr_64x64",
                withExtension: "h264",
                subdirectory: "Fixtures"
            ),
            "single_idr_64x64.h264 fixture missing from the test bundle"
        )
        return try Data(contentsOf: url)
    }

    private func packet(seq: UInt32, data: Data, keyframe: Bool) -> FramePacket {
        FramePacket(
            seq: seq,
            timestampUs: UInt64(seq) * 16_667,
            data: data,
            width: 64,
            height: 64,
            isKeyframe: keyframe
        )
    }

    func testDecodesSingleIDRFixtureToNV12() throws {
        let data = try fixtureData()
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }

        let decoded = expectation(description: "one frame decoded")
        var format: OSType = 0
        var width = 0
        var height = 0
        decoder.onFrameDecoded = { pixelBuffer, _ in
            format = CVPixelBufferGetPixelFormatType(pixelBuffer)
            width = CVPixelBufferGetWidth(pixelBuffer)
            height = CVPixelBufferGetHeight(pixelBuffer)
            decoded.fulfill()
        }

        decoder.decode(packet: packet(seq: 1, data: data, keyframe: true))
        wait(for: [decoded], timeout: 10)

        XCTAssertEqual(width, 64)
        XCTAssertEqual(height, 64)
        XCTAssertEqual(
            format, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            "decoder must output NV12 for the Metal YCbCr path"
        )
    }

    func testRepeatedDecodeDoesNotRebuildSessionPerKeyframe() throws {
        // The old per-IDR session recreation produced a "VideoToolbox session
        // ready" event for every keyframe. Now only the first format
        // description may create a session; identical SPS/PPS must not.
        let data = try fixtureData()
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }

        let sessionEvents = OSAllocatedUnfairLock(initialState: 0)
        decoder.onEvent = { message in
            if message.hasPrefix("VideoToolbox session ready") {
                sessionEvents.withLock { $0 += 1 }
            }
        }
        let decodedAll = expectation(description: "frames decoded")
        decodedAll.expectedFulfillmentCount = 5
        decoder.onFrameDecoded = { _, _ in decodedAll.fulfill() }

        for seq in 1...5 {
            decoder.decode(packet: packet(seq: UInt32(seq), data: data, keyframe: true))
        }
        wait(for: [decodedAll], timeout: 10)

        XCTAssertEqual(
            sessionEvents.withLock { $0 }, 1,
            "identical keyframes must reuse one VTDecompressionSession"
        )
    }

    func testShutdownDuringDecodeStressDoesNotCrash() throws {
        let data = try fixtureData()
        for _ in 0..<10 {
            let decoder = VideoDecoder()
            for seq in 1...3 {
                decoder.decode(packet: packet(seq: UInt32(seq), data: data, keyframe: true))
            }
            let done = expectation(description: "shutdown completed")
            decoder.shutdown { done.fulfill() }
            wait(for: [done], timeout: 5)
        }
    }

    func testBadAccessUnitRequestsOneKeyframeAndRecoversInTheSameSession() throws {
        let data = try fixtureData()
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }
        let first = expectation(description: "initial frame")
        let request = expectation(description: "decode error requests a keyframe")
        let recovered = expectation(description: "recovery frame")
        let sessionEvents = OSAllocatedUnfairLock(initialState: 0)
        let requests = OSAllocatedUnfairLock(initialState: 0)
        decoder.onEvent = { message in
            if message.hasPrefix("VideoToolbox session ready") {
                sessionEvents.withLock { $0 += 1 }
            }
        }
        decoder.onFrameDecoded = { _, timestamp in
            if timestamp == 16_667 { first.fulfill() }
            if timestamp == 20 * 16_667 { recovered.fulfill() }
        }
        decoder.onNeedsKeyframe = {
            requests.withLock { $0 += 1 }
            request.fulfill()
        }
        decoder.decode(packet: packet(seq: 1, data: data, keyframe: true))
        wait(for: [first], timeout: 10)

        // A truncated P-slice reaches the real VideoToolbox callback. A
        // complete wire frame can still contain undecodable codec data.
        let corrupt = Data([0, 0, 0, 1, 0x41, 0x80])
        decoder.decode(packet: packet(seq: 2, data: corrupt, keyframe: false))
        guard XCTWaiter.wait(for: [request], timeout: 3) == .completed else {
            XCTFail("VideoToolbox rejected a frame without requesting recovery")
            return
        }
        for seq in 3...10 {
            decoder.decode(packet: packet(seq: UInt32(seq), data: corrupt, keyframe: false))
        }
        decoder.decode(packet: packet(seq: 20, data: data, keyframe: true))
        wait(for: [recovered], timeout: 10)
        XCTAssertEqual(requests.withLock { $0 }, 1, "wait for a sync sample without a request storm")
        XCTAssertEqual(sessionEvents.withLock { $0 }, 1, "bad data must not rebuild a healthy session")
    }

    func testKeyframeThatFailsToDecodeRebuildsTheSession() throws {
        // Hosted CI caught a software session that rejected every later frame,
        // including each replacement IDR, so requesting keyframes never helped.
        let data = try fixtureData()
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }
        let first = expectation(description: "initial frame")
        let request = expectation(description: "failed keyframe requests another")
        let recovered = expectation(description: "recovery frame")
        let sessionEvents = OSAllocatedUnfairLock(initialState: 0)
        decoder.onEvent = { message in
            if message.hasPrefix("VideoToolbox session ready") {
                sessionEvents.withLock { $0 += 1 }
            }
        }
        decoder.onFrameDecoded = { _, timestamp in
            if timestamp == 16_667 { first.fulfill() }
            if timestamp == 20 * 16_667 { recovered.fulfill() }
        }
        decoder.onNeedsKeyframe = { request.fulfill() }
        decoder.decode(packet: packet(seq: 1, data: data, keyframe: true))
        wait(for: [first], timeout: 10)

        // An IDR slice whose data cannot be decoded.
        let corruptIDR = Data([0, 0, 0, 1, 0x65, 0x80])
        decoder.decode(packet: packet(seq: 2, data: corruptIDR, keyframe: true))
        guard XCTWaiter.wait(for: [request], timeout: 3) == .completed else {
            XCTFail("VideoToolbox rejected a keyframe without requesting recovery")
            return
        }
        decoder.decode(packet: packet(seq: 20, data: data, keyframe: true))
        wait(for: [recovered], timeout: 10)
        XCTAssertEqual(sessionEvents.withLock { $0 }, 2, "a failed keyframe must rebuild the session")
    }
}
