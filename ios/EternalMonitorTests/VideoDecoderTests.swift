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

    /// A keyframe and one frame that depends on it, from the same encode.
    private func gopFixture() throws -> (idr: Data, p: Data) {
        func load(_ name: String) throws -> Data {
            let url = try XCTUnwrap(
                Bundle(for: VideoDecoderTests.self).url(forResource: name, withExtension: "h264", subdirectory: "Fixtures"),
                "\(name).h264 fixture missing from the test bundle"
            )
            return try Data(contentsOf: url)
        }
        return (try load("gop_idr_64x64"), try load("gop_p_64x64"))
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

    func testDependentFrameDecodesNormally() throws {
        // Control for the test below: without a loss the P-frame is shown.
        let gop = try gopFixture()
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }
        let frames = expectation(description: "keyframe and P-frame decoded")
        frames.expectedFulfillmentCount = 2
        decoder.onFrameDecoded = { _, _ in frames.fulfill() }
        decoder.decode(packet: packet(seq: 1, data: gop.idr, keyframe: true))
        decoder.decode(packet: packet(seq: 2, data: gop.p, keyframe: false))
        wait(for: [frames], timeout: 10)
    }

    func testFrameAfterALostReferenceIsHeldUntilTheNextKeyframe() throws {
        // The reassembler dropped the frame before seq 2. Decoding seq 2
        // would draw it against the wrong reference (smeared blocks) or make
        // VideoToolbox fail with -12909, as 0.3.1 iPads reported.
        let gop = try gopFixture()
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }
        let shown = OSAllocatedUnfairLock(initialState: [UInt64]())
        let errors = OSAllocatedUnfairLock(initialState: 0)
        let first = expectation(description: "first keyframe")
        let resumed = expectation(description: "frames after the next keyframe")
        resumed.expectedFulfillmentCount = 2
        decoder.onFrameDecoded = { _, timestamp in
            shown.withLock { $0.append(timestamp / 16_667) }
            if timestamp == 16_667 { first.fulfill() }
            if timestamp >= 3 * 16_667 { resumed.fulfill() }
        }
        decoder.onEvent = { message in
            if message.hasPrefix("Decoder callback") || message.hasPrefix("VTDecodeFrame") {
                errors.withLock { $0 += 1 }
            }
        }
        decoder.decode(packet: packet(seq: 1, data: gop.idr, keyframe: true))
        wait(for: [first], timeout: 10)

        decoder.referenceLost()
        decoder.decode(packet: packet(seq: 2, data: gop.p, keyframe: false))
        decoder.decode(packet: packet(seq: 3, data: gop.idr, keyframe: true))
        decoder.decode(packet: packet(seq: 4, data: gop.p, keyframe: false))
        wait(for: [resumed], timeout: 10)

        XCTAssertEqual(shown.withLock { $0 }, [1, 3, 4], "the frame after the loss must not be shown")
        XCTAssertEqual(errors.withLock { $0 }, 0)
        XCTAssertNil(decoder.keyframeWaitUs, "a keyframe ends the wait")
    }

    func testHeldPictureAsksAgainForAKeyframeOncePerSecond() throws {
        // The reassembler's request, or the host's keyframe, can be lost too.
        // Before this the decoder dropped every dependent frame silently and
        // the picture stayed frozen until the host's next periodic keyframe.
        let gop = try gopFixture()
        let clock = OSAllocatedUnfairLock<UInt64>(initialState: 5_000_000)
        let decoder = VideoDecoder(nowUs: { clock.withLock { $0 } })
        defer { decoder.shutdown() }
        let requests = OSAllocatedUnfairLock(initialState: 0)
        let events = OSAllocatedUnfairLock(initialState: [String]())
        decoder.onNeedsKeyframe = { requests.withLock { $0 += 1 } }
        decoder.onEvent = { message in events.withLock { $0.append(message) } }
        let first = expectation(description: "first keyframe")
        let restored = expectation(description: "keyframe after the wait")
        decoder.onFrameDecoded = { _, timestamp in
            if timestamp == 16_667 { first.fulfill() }
            if timestamp == 9 * 16_667 { restored.fulfill() }
        }
        decoder.decode(packet: packet(seq: 1, data: gop.idr, keyframe: true))
        wait(for: [first], timeout: 10)

        decoder.referenceLost()
        let holding = expectation(for: NSPredicate { _, _ in decoder.keyframeWaitUs != nil }, evaluatedWith: nil)
        wait(for: [holding], timeout: 5)

        func dependentFrame(at seconds: Double, seq: UInt32) {
            clock.withLock { $0 = 5_000_000 + UInt64(seconds * 1_000_000) }
            decoder.decode(packet: packet(seq: seq, data: gop.p, keyframe: false))
        }
        func waitForRequests(_ count: Int) {
            let met = expectation(for: NSPredicate { _, _ in requests.withLock { $0 } >= count }, evaluatedWith: nil)
            wait(for: [met], timeout: 5)
        }
        dependentFrame(at: 0.5, seq: 2)
        dependentFrame(at: 1.1, seq: 3)
        waitForRequests(1)
        dependentFrame(at: 1.5, seq: 4)
        dependentFrame(at: 2.2, seq: 5)
        waitForRequests(2)
        XCTAssertEqual(decoder.keyframeWaitUs, 2_200_000)

        decoder.decode(packet: packet(seq: 9, data: gop.idr, keyframe: true))
        wait(for: [restored], timeout: 10)
        XCTAssertEqual(requests.withLock { $0 }, 2)
        XCTAssertNil(decoder.keyframeWaitUs)
        XCTAssertTrue(events.withLock { $0 }.contains("Picture held 2200 ms waiting for a keyframe"))
    }

    private func fixture(_ name: String, _ ext: String) throws -> Data {
        let url = try XCTUnwrap(Bundle(for: VideoDecoderTests.self).url(forResource: name, withExtension: ext, subdirectory: "Fixtures"),
                                "\(name).\(ext) fixture missing from the test bundle")
        return try Data(contentsOf: url)
    }

    /// A keyframe whose parameter sets all differ must become one new format.
    /// Adopting each parameter set as it was parsed paired the new SPS with
    /// the old PPS (and, for HEVC, the new VPS with the old SPS and PPS).
    /// On 0.3.1 an iPad switching 800x600 to 2560x1440 logged the new size
    /// "accepted by the live session" of the 800x600 decoder, then glitched.
    private func assertResolutionChangeIsOneFormat(first: Data, second: Data, file: StaticString = #filePath, line: UInt = #line) {
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }
        let events = OSAllocatedUnfairLock(initialState: [String]())
        decoder.onEvent = { message in events.withLock { $0.append(message) } }
        let firstFrame = expectation(description: "first size")
        let secondFrame = expectation(description: "second size")
        decoder.onFrameDecoded = { buffer, timestamp in
            if timestamp == 16_667 { firstFrame.fulfill() }
            if timestamp == 2 * 16_667, CVPixelBufferGetWidth(buffer) == 96 { secondFrame.fulfill() }
        }
        decoder.decode(packet: packet(seq: 1, data: first, keyframe: true))
        wait(for: [firstFrame], timeout: 10)
        let before = events.withLock { $0.count }
        decoder.decode(packet: packet(seq: 2, data: second, keyframe: true))
        wait(for: [secondFrame], timeout: 10)
        let change = events.withLock { Array($0[before...]) }
        let formats = change.filter { $0.hasPrefix("Updated format description") }
        XCTAssertEqual(formats.count, 1, "one keyframe, one format: \(formats)", file: file, line: line)
        XCTAssertTrue(formats.first?.contains("size=96x64") == true, file: file, line: line)
        XCTAssertFalse(change.contains { $0.hasPrefix("Format change accepted") }, "a new size needs a new session", file: file, line: line)
        XCTAssertEqual(change.filter { $0.hasPrefix("VideoToolbox session ready") }.count, 1, file: file, line: line)
    }

    func testH264ResolutionChangeAdoptsSPSAndPPSTogether() throws {
        assertResolutionChangeIsOneFormat(first: try gopFixture().idr, second: try fixture("idr_96x64", "h264"))
    }

    func testHEVCResolutionChangeAdoptsVPSSPSAndPPSTogether() throws {
        assertResolutionChangeIsOneFormat(first: try fixture("single_irap_64x64", "h265"), second: try fixture("irap_96x64", "h265"))
    }

    func testDependentFrameBeforeTheFirstKeyframeAsksForOne() throws {
        // The host's first keyframe was lost on Wi-Fi: one 0.3.1 iPad dropped
        // two seconds of frames "because decoder is not ready" and never asked.
        let gop = try gopFixture()
        let decoder = VideoDecoder()
        defer { decoder.shutdown() }
        let requests = OSAllocatedUnfairLock(initialState: 0)
        let asked = expectation(description: "keyframe requested")
        decoder.onNeedsKeyframe = {
            requests.withLock { $0 += 1 }
            asked.fulfill()
        }
        let shown = expectation(description: "keyframe decoded")
        decoder.onFrameDecoded = { _, timestamp in if timestamp == 3 * 16_667 { shown.fulfill() } }
        decoder.decode(packet: packet(seq: 1, data: gop.p, keyframe: false))
        decoder.decode(packet: packet(seq: 2, data: gop.p, keyframe: false))
        decoder.decode(packet: packet(seq: 3, data: gop.idr, keyframe: true))
        wait(for: [asked, shown], timeout: 10)
        XCTAssertEqual(requests.withLock { $0 }, 1, "one request, not one per frame")
    }
}
