import XCTest
import CoreVideo
import VideoToolbox
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

    private func packet(seq: UInt32, data: Data, keyframe: Bool, epoch: UInt32 = 0) -> FramePacket {
        FramePacket(
            seq: seq,
            timestampUs: UInt64(seq) * 16_667,
            data: data,
            width: 64,
            height: 64,
            isKeyframe: keyframe,
            streamEpoch: epoch
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

    // MARK: - Reference loss, from the bitstream

    /// Eight access units, IDR P P P IDR P P P, as an encoder sends them.
    private func twoGOPs(_ ext: String) throws -> [Data] {
        let data = try fixture("gop2_64x64", ext)
        var starts: [(offset: Int, payload: Int)] = []
        var i = 0
        while i + 3 <= data.count {
            if data[i] == 0, data[i + 1] == 0, data[i + 2] == 1 {
                starts.append((i > 0 && data[i - 1] == 0 ? i - 1 : i, i + 3)); i += 3
            } else { i += 1 }
        }
        var units: [Data] = []
        var pending = Data()
        for (n, start) in starts.enumerated() {
            let end = n + 1 < starts.count ? starts[n + 1].offset : data.count
            let nal = data[start.offset..<end]
            let header = data[start.payload]
            let isSlice = ext == "h264" ? [1, 5].contains(header & 0x1F) : (header >> 1) & 0x3F <= 31
            pending.append(nal)
            if isSlice { units.append(pending); pending = Data() }
        }
        XCTAssertEqual(units.count, 8)
        return units
    }

    private struct Run {
        var shown: [UInt32] = []
        var requests = 0
        var errors = 0
        var events: [String] = []
    }

    /// Decodes (seq, access unit index) pairs and reports which seqs were shown.
    private func run(_ ext: String, _ frames: [(UInt32, Int)], decoder: VideoDecoder = VideoDecoder()) throws -> (Run, VideoDecoder) {
        try run(ext, frames.map { ($0.0, $0.1, 0) }, decoder: decoder)
    }

    /// The same, with each frame's stream epoch.
    private func run(_ ext: String, _ frames: [(UInt32, Int, UInt32)], decoder: VideoDecoder = VideoDecoder()) throws -> (Run, VideoDecoder) {
        let units = try twoGOPs(ext)
        let state = OSAllocatedUnfairLock(initialState: Run())
        decoder.onFrameDecoded = { _, timestamp in state.withLock { $0.shown.append(UInt32(timestamp / 16_667)) } }
        decoder.onNeedsKeyframe = { state.withLock { $0.requests += 1 } }
        decoder.onEvent = { message in
            state.withLock {
                $0.events.append(message)
                if message.hasPrefix("Decoder callback") || message.hasPrefix("VTDecodeFrame") { $0.errors += 1 }
            }
        }
        for (seq, index, epoch) in frames {
            decoder.decode(packet: packet(seq: seq, data: units[index], keyframe: index % 4 == 0, epoch: epoch))
        }
        let drained = expectation(description: "decode queue drained")
        decoder.shutdown { drained.fulfill() }
        wait(for: [drained], timeout: 10)
        return (state.withLock { $0 }, decoder)
    }

    func testFrameAfterAWholeLostFrameIsHeldUntilTheNextKeyframe() throws {
        // seq 3 never reached the iPad, not even one fragment, so the
        // reassembler counted nothing. Decoding seq 4 would draw it against
        // the wrong reference: smeared blocks or -12909.
        for ext in ["h264", "h265"] {
            let (result, _) = try run(ext, [(1, 0), (2, 1), (4, 3), (5, 4), (6, 5)])
            XCTAssertEqual(result.shown, [1, 2, 5, 6], ext)
            XCTAssertEqual(result.requests, 1, "\(ext): a lost frame asks for a keyframe at once")
            XCTAssertEqual(result.errors, 0, ext)
        }
    }

    func testHostSkippingFrameNumbersIsNotALoss() throws {
        // The host numbers frames as captured and drops captured frames its
        // encoder had no time for, so frame_seq skips with nothing lost.
        for ext in ["h264", "h265"] {
            let (result, decoder) = try run(ext, [(1, 0), (2, 1), (5, 2), (9, 3), (10, 4), (14, 5), (15, 6)])
            XCTAssertEqual(result.shown, [1, 2, 5, 9, 10, 14, 15], ext)
            XCTAssertEqual(result.requests, 0, ext)
            XCTAssertNil(decoder.keyframeWaitUs, ext)
        }
    }

    func testLosslessStreamIsNeverHeld() throws {
        // USB is a TCP tunnel: every frame arrives, in order.
        for ext in ["h264", "h265"] {
            let (result, _) = try run(ext, (0..<8).map { (UInt32($0 + 1), $0) })
            XCTAssertEqual(result.shown, Array(1...8), ext)
            XCTAssertEqual(result.requests, 0, ext)
        }
    }

    func testLostKeyframeHoldsTheFramesThatFollowIt() throws {
        // The replacement keyframe (seq 3) was dropped unfinished; seq 4
        // depends on it, not on seq 2.
        for ext in ["h264", "h265"] {
            let (result, _) = try run(ext, [(1, 0), (2, 1), (4, 5), (5, 6), (6, 4), (7, 5)])
            XCTAssertEqual(result.shown, [1, 2, 6, 7], ext)
            XCTAssertEqual(result.errors, 0, ext)
        }
    }

    func testStreamRestartWithoutItsKeyframeIsHeld() throws {
        // A new stream epoch numbers from 1 again; its first keyframe was lost.
        for ext in ["h264", "h265"] {
            let (result, _) = try run(ext, [(7, 0), (8, 1), (1, 1), (2, 2), (3, 4), (4, 5)])
            XCTAssertEqual(result.shown, [7, 8, 3, 4], ext)
        }
    }

    func testRecoveredPictureIsNotHeldByALossCountedLater() throws {
        // Sol's sequence: keyframe 1, frame 2 arrives partly, the replacement
        // keyframe 3, then frame 4. Without repair (USB, older PCs) the
        // reassembler delivers 3 as soon as it completes and only then counts
        // 2 as dropped, after 3 has already restored the picture. The first
        // counter-based detector then held 4: [1, 3] and a frozen picture.
        for ext in ["h264", "h265"] {
            let units = try twoGOPs(ext)
            let assembler = FrameAssembler()
            let decoder = VideoDecoder()
            let shown = OSAllocatedUnfairLock(initialState: [UInt32]())
            decoder.onFrameDecoded = { _, timestamp in shown.withLock { $0.append(UInt32(timestamp / 16_667)) } }
            // The same hand-off as ConnectionManager's.
            assembler.onFrameAssembled = { data, seq, _, isKeyframe in
                decoder.decode(packet: self.packet(seq: seq, data: data, keyframe: isKeyframe))
            }
            func send(_ seq: UInt32, _ data: Data, keyframe: Bool, fragments: [Int]? = nil) {
                let size = max(1, (data.count + 1) / 2)
                let count = (data.count + size - 1) / size
                for index in fragments ?? Array(0..<count) {
                    let chunk = data[data.startIndex + index * size..<min(data.startIndex + (index + 1) * size, data.endIndex)]
                    assembler.addFragment(seq: seq, index: UInt16(index), count: UInt16(count), epoch: 1,
                                          isKeyframe: keyframe, captureTimestampUs: 0, payload: Data(chunk))
                }
            }
            send(1, units[0], keyframe: true)
            send(2, units[1], keyframe: false, fragments: [1])
            send(3, units[4], keyframe: true)
            send(4, units[5], keyframe: false)
            let drained = expectation(description: "decoded")
            decoder.shutdown { drained.fulfill() }
            wait(for: [drained], timeout: 10)
            XCTAssertEqual(assembler.counters.withLock { $0.framesDropped }, 1, ext)
            XCTAssertEqual(shown.withLock { $0 }, [1, 3, 4], ext)
        }
    }

    func testNewEpochWithoutItsKeyframeIsHeld() throws {
        // Sol: epoch 1's keyframe is seq 1; the PC restarts its stream and
        // epoch 2's keyframe is lost, so its first frame arrives as seq 2.
        // Its numbers happen to follow on, but it references a picture of the
        // new stream the iPad never had.
        for ext in ["h264", "h265"] {
            let (result, _) = try run(ext, [(1, 0, 1), (2, 5, 2), (3, 4, 2), (4, 5, 2)])
            XCTAssertEqual(result.shown, [1, 3, 4], ext)
            XCTAssertEqual(result.requests, 1, ext)
        }
    }

    func testContiguouslyNumberedPCsLostReopenKeyframeIsHeld() throws {
        // The PC reopened its encoder; the new keyframe (seq 2) was lost and
        // the new encoder's first P-frame (frame_num 1) follows the old
        // keyframe's count. Only contiguous numbering shows the loss.
        let numbered = VideoDecoder()
        numbered.setFrameSeqContiguous(true)
        let (held, _) = try run("h264", [(1, 0), (3, 5), (4, 4), (5, 5)], decoder: numbered)
        XCTAssertEqual(held.shown, [1, 4, 5])
        XCTAssertEqual(held.requests, 1)
        let (shown, _) = try run("h264", [(1, 0), (3, 5), (4, 4), (5, 5)])
        XCTAssertEqual(shown.shown, [1, 3, 4, 5], "a 0.3.1 PC's numbers skip on their own")
    }

    func testSilentStreamStillRetriesThenRestartsThenReconnects() throws {
        // Sol: recovery must not wait for frames to arrive. A lost frame, then
        // nothing at all: retry every 500 ms, restart the stream at 2 s, give
        // the session up at 5 s, with the last clean picture on screen.
        let units = try twoGOPs("h264")
        let clock = OSAllocatedUnfairLock<UInt64>(initialState: 5_000_000)
        let decoder = VideoDecoder(nowUs: { clock.withLock { $0 } })
        defer { decoder.shutdown() }
        struct Calls { var requests = 0; var restarts = 0; var stalls = 0; var events: [String] = [] }
        let calls = OSAllocatedUnfairLock(initialState: Calls())
        decoder.onNeedsKeyframe = { calls.withLock { $0.requests += 1 } }
        decoder.onRestartStream = { calls.withLock { $0.restarts += 1 } }
        decoder.onStalled = { calls.withLock { $0.stalls += 1 } }
        decoder.onEvent = { message in calls.withLock { $0.events.append(message) } }
        let restored = expectation(description: "keyframe after the stall")
        decoder.onFrameDecoded = { _, timestamp in if timestamp == 20 * 16_667 { restored.fulfill() } }
        func at(_ seconds: Double) { clock.withLock { $0 = 5_000_000 + UInt64(seconds * 1_000_000) } }
        func until(_ condition: @escaping (Calls) -> Bool) {
            let met = expectation(for: NSPredicate { _, _ in calls.withLock { condition($0) } }, evaluatedWith: nil)
            wait(for: [met], timeout: 10)
        }
        for (seq, index) in [(1, 0), (2, 1), (4, 3)] {
            decoder.decode(packet: packet(seq: UInt32(seq), data: units[index], keyframe: index == 0))
        }
        until { $0.requests == 1 }                       // at once, on the lost frame
        at(0.3); Thread.sleep(forTimeInterval: 0.3)
        XCTAssertEqual(calls.withLock { $0.requests }, 1, "not before 500 ms")
        at(0.55); until { $0.requests == 2 }             // no frame arrived
        at(1.1); until { $0.requests == 3 }
        at(2.05); until { $0.restarts == 1 }
        XCTAssertEqual(calls.withLock { $0.stalls }, 0)
        at(2.6); until { $0.requests == 4 }              // retries continue after the restart
        at(5.05); until { $0.stalls == 1 }
        at(6.0); Thread.sleep(forTimeInterval: 0.3)
        XCTAssertEqual(calls.withLock { $0.restarts }, 1, "each step once per hold")
        XCTAssertEqual(calls.withLock { $0.stalls }, 1, "each step once per hold")
        XCTAssertNotNil(decoder.keyframeWaitUs)
        // A keyframe still recovers: the rebuilt decoder opens from its parameter sets.
        decoder.decode(packet: packet(seq: 20, data: units[4], keyframe: true))
        wait(for: [restored], timeout: 10)
        XCTAssertNil(decoder.keyframeWaitUs)
        XCTAssertTrue(calls.withLock { $0.events }.contains { $0.hasPrefix("Picture held 6000 ms") })
    }

    func testKeyframeThatFailsToDecodeContinuesTheHold() throws {
        // Progress means a decoded picture. A keyframe VideoToolbox rejects
        // must not restart the 2 s and 5 s clock, or a stream of bad
        // keyframes would never escalate.
        let units = try twoGOPs("h264")
        let clock = OSAllocatedUnfairLock<UInt64>(initialState: 5_000_000)
        let decoder = VideoDecoder(nowUs: { clock.withLock { $0 } })
        defer { decoder.shutdown() }
        let failed = expectation(description: "keyframe rejected")
        decoder.onEvent = { message in if message.hasPrefix("Decoder callback") { failed.fulfill() } }
        for (seq, index) in [(1, 0), (2, 1), (4, 3)] {
            decoder.decode(packet: packet(seq: UInt32(seq), data: units[index], keyframe: index == 0))
        }
        let holding = expectation(for: NSPredicate { _, _ in decoder.keyframeWaitUs != nil }, evaluatedWith: nil)
        wait(for: [holding], timeout: 5)
        clock.withLock { $0 = 6_000_000 }
        decoder.decode(packet: packet(seq: 5, data: Data([0, 0, 0, 1, 0x65, 0x80]), keyframe: true))
        wait(for: [failed], timeout: 10)
        let settled = expectation(for: NSPredicate { _, _ in decoder.keyframeWaitUs == 1_000_000 }, evaluatedWith: nil)
        wait(for: [settled], timeout: 5)
    }

    // MARK: - A session that fails to open

    private func failingSessions(_ failures: Int) -> (VideoDecoder.SessionFactory, OSAllocatedUnfairLock<Int>) {
        let attempts = OSAllocatedUnfairLock(initialState: 0)
        let factory: VideoDecoder.SessionFactory = { format, specification, attributes in
            let attempt = attempts.withLock { value -> Int in value += 1; return value }
            if attempt <= failures { return (kVTVideoDecoderMalfunctionErr, nil) }
            var session: VTDecompressionSession?
            let status = VTDecompressionSessionCreate(allocator: kCFAllocatorDefault, formatDescription: format,
                decoderSpecification: specification, imageBufferAttributes: attributes,
                outputCallback: nil, decompressionSessionOut: &session)
            return (status, session)
        }
        return (factory, attempts)
    }

    func testFailedSessionIsRetriedOnTheNextKeyframe() throws {
        // Sol injected one failure, then four valid keyframes: one attempt
        // and no picture, because the failed format stayed cached.
        let (factory, attempts) = failingSessions(3)
        let (result, _) = try run("h264", [(1, 0), (2, 1), (3, 4), (4, 0), (5, 4), (6, 5)],
                                  decoder: VideoDecoder(makeSession: factory))
        XCTAssertEqual(attempts.withLock { $0 }, 4, "one attempt per keyframe, none per dependent frame")
        XCTAssertEqual(result.shown, [5, 6])
        XCTAssertEqual(result.events.filter { $0.hasPrefix("Failed to create VideoToolbox session") }.count, 1,
                       "the outage is logged once")
        XCTAssertTrue(result.events.contains { $0.contains("after 3 failed attempts") })
        XCTAssertEqual(result.events.filter { $0.hasPrefix("Dropped access unit") }.count, 1)
    }

    func testSessionThatFailsMidStreamHoldsThePictureAndAsksForAKeyframe() throws {
        // A resolution change whose new session will not open: the last
        // picture stays, "Restoring the picture" shows, and the iPad keeps
        // asking until a keyframe opens a session.
        let first = try gopFixture()
        let bigger = try fixture("idr_96x64", "h264")
        let attempts = OSAllocatedUnfairLock(initialState: 0)
        let decoder = VideoDecoder(makeSession: { format, specification, attributes in
            let attempt = attempts.withLock { value -> Int in value += 1; return value }
            if attempt == 2 { return (kVTVideoDecoderMalfunctionErr, nil) }
            var session: VTDecompressionSession?
            let status = VTDecompressionSessionCreate(allocator: kCFAllocatorDefault, formatDescription: format,
                decoderSpecification: specification, imageBufferAttributes: attributes,
                outputCallback: nil, decompressionSessionOut: &session)
            return (status, session)
        })
        defer { decoder.shutdown() }
        let requests = OSAllocatedUnfairLock(initialState: 0)
        decoder.onNeedsKeyframe = { requests.withLock { $0 += 1 } }
        let shown = expectation(description: "first picture")
        let resized = expectation(description: "picture after the retry")
        decoder.onFrameDecoded = { buffer, timestamp in
            if timestamp == 16_667 { shown.fulfill() }
            if timestamp == 9 * 16_667, CVPixelBufferGetWidth(buffer) == 96 { resized.fulfill() }
        }
        decoder.decode(packet: packet(seq: 1, data: first.idr, keyframe: true))
        wait(for: [shown], timeout: 10)
        decoder.decode(packet: packet(seq: 2, data: bigger, keyframe: true))
        decoder.decode(packet: packet(seq: 3, data: first.p, keyframe: false))
        let holding = expectation(for: NSPredicate { _, _ in decoder.keyframeWaitUs != nil && requests.withLock { $0 } >= 1 },
                                  evaluatedWith: nil)
        wait(for: [holding], timeout: 10)
        decoder.decode(packet: packet(seq: 9, data: bigger, keyframe: true))
        wait(for: [resized], timeout: 10)
        XCTAssertEqual(attempts.withLock { $0 }, 3)
        XCTAssertNil(decoder.keyframeWaitUs)
    }
}
