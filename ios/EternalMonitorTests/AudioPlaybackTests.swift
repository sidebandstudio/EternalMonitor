import AVFoundation
import XCTest
@testable import EternalMonitor

final class AudioPlayerTests: XCTestCase {
    private final class BlockingOutput: AudioOutputDevice {
        let entered: XCTestExpectation
        let release = DispatchSemaphore(value: 0)
        let stopped = DispatchSemaphore(value: 0)
        let error: Error?
        init(entered: XCTestExpectation, error: Error? = nil) {
            self.entered = entered; self.error = error
        }
        func start(pcm: AudioPCMBuffer) throws {
            entered.fulfill()
            _ = release.wait(timeout: .now() + 20)
            if let error { throw error }
        }
        func stop(deactivate: Bool) { stopped.signal() }
    }

    func testSlowOutputStartupDoesNotBlockPacketDecoding() throws {
        let output = BlockingOutput(entered: expectation(description: "output startup entered"))
        let player = AudioPlayer(output: output)
        defer { output.release.signal(); player.stop() }
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "tone_1khz_20ms",
            withExtension: "opus", subdirectory: "Fixtures"))
        let opus = try Data(contentsOf: url)
        func send(_ seq: UInt32) {
            player.receive(header: AudioHeader(sessionId: 7, streamEpoch: 1, audioSeq: seq,
                captureTimestampUs: UInt64(seq) * 20_000, discontinuity: seq == 1), opus: opus)
        }
        send(1)
        wait(for: [output.entered], timeout: 2)
        for seq in 2...12 {
            send(UInt32(seq))
            let deadline = Date().addingTimeInterval(1)
            while player.stats.decoded < UInt64(seq), Date() < deadline {
                Thread.sleep(forTimeInterval: 0.005)
            }
            guard player.stats.decoded == UInt64(seq) else {
                XCTFail("Packet \(seq) waited for output startup: \(player.stats)")
                return
            }
        }
        XCTAssertEqual(player.stats.lost, 0)
        XCTAssertLessThanOrEqual(player.stats.bufferMs, 80, "waiting for output must not build latency")
        XCTAssertFalse(player.stats.playing)
    }

    func testMuteIgnoresFailureFromAnOlderOutputStart() throws {
        let output = BlockingOutput(entered: expectation(description: "output startup entered"),
            error: NSError(domain: "AudioOutputTests", code: 1))
        let player = AudioPlayer(output: output)
        defer { output.release.signal(); player.stop() }
        let failed = expectation(description: "retired output must not report an error")
        failed.isInverted = true
        player.onError = { _ in failed.fulfill() }
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "tone_1khz_20ms",
            withExtension: "opus", subdirectory: "Fixtures"))
        player.receive(header: AudioHeader(sessionId: 7, streamEpoch: 1, audioSeq: 1,
            captureTimestampUs: 20_000, discontinuity: true), opus: try Data(contentsOf: url))
        wait(for: [output.entered], timeout: 2)
        let decoded = Date().addingTimeInterval(1)
        while player.stats.decoded == 0, Date() < decoded { Thread.sleep(forTimeInterval: 0.005) }
        XCTAssertEqual(player.stats.bufferMs, 20)
        player.setEnabled(false)
        let muted = Date().addingTimeInterval(1)
        while player.stats.bufferMs != 0, Date() < muted { Thread.sleep(forTimeInterval: 0.005) }
        XCTAssertEqual(player.stats.bufferMs, 0)
        output.release.signal()
        XCTAssertEqual(output.stopped.wait(timeout: .now() + 2), .success)
        wait(for: [failed], timeout: 0.1)
        XCTAssertNil(player.stats.error)
        XCTAssertFalse(player.stats.playing)
    }

    func testStopWaitsForPendingOutputToCloseBeforeReturning() throws {
        let output = BlockingOutput(entered: expectation(description: "output startup entered"))
        let player = AudioPlayer(output: output)
        defer { output.release.signal(); player.stop() }
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "tone_1khz_20ms",
            withExtension: "opus", subdirectory: "Fixtures"))
        player.receive(header: AudioHeader(sessionId: 7, streamEpoch: 1, audioSeq: 1,
            captureTimestampUs: 20_000, discontinuity: true), opus: try Data(contentsOf: url))
        wait(for: [output.entered], timeout: 2)
        let stopped = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { player.stop(); stopped.signal() }
        XCTAssertEqual(stopped.wait(timeout: .now() + 0.05), .timedOut)
        output.release.signal()
        XCTAssertEqual(stopped.wait(timeout: .now() + 2), .success)
        XCTAssertEqual(output.stopped.wait(timeout: .now() + 1), .success)
        XCTAssertEqual(player.stats, AudioStats())
    }
}

final class AudioPCMBufferTests: XCTestCase {
    private func block(_ value: Float) -> [Float] { [Float](repeating: value, count: 1920) }
    private func render(_ ring: AudioPCMBuffer, frames: Int, at now: UInt64) -> [Float] {
        let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 2)!
        let out = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: UInt32(frames))!
        out.frameLength = UInt32(frames)
        ring.render(frames: frames, buffers: UnsafeMutableAudioBufferListPointer(out.mutableAudioBufferList), nowUs: now)
        let channels = out.floatChannelData!
        return (0..<frames).flatMap { [channels[0][$0], channels[1][$0]] }
    }

    func testWaitsForSixtyMillisecondsAndRendersStereoInOrder() {
        let ring = AudioPCMBuffer()
        ring.append(block(0.1)); ring.append(block(0.2))
        XCTAssertEqual(render(ring, frames: 480, at: 0), [Float](repeating: 0, count: 960))
        XCTAssertEqual(ring.snapshot.bufferMs, 40)
        ring.append(block(0.3))
        XCTAssertEqual(render(ring, frames: 960, at: 20_000), block(0.1))
        XCTAssertEqual(render(ring, frames: 960, at: 40_000), block(0.2))
        XCTAssertEqual(ring.snapshot.underruns, 0)
    }

    func testOverflowDropsTheOldestTwentyMilliseconds() {
        let ring = AudioPCMBuffer()
        for i in 1...5 { ring.append(block(Float(i) / 10)) }
        XCTAssertEqual(ring.snapshot.bufferMs, 80)
        XCTAssertEqual(ring.snapshot.overruns, 1)
        XCTAssertEqual(render(ring, frames: 960, at: 0), block(0.2))
    }

    func testTwoUnderrunsGrowTargetAndThirtyCleanSecondsShrinkIt() {
        let ring = AudioPCMBuffer()
        _ = render(ring, frames: 960, at: 0)
        XCTAssertEqual(ring.snapshot.underruns, 0, "initial buffering is not an underrun")
        for _ in 0..<3 { ring.append(block(0.1)) }
        _ = render(ring, frames: 2881, at: 1_000_000)
        XCTAssertEqual(ring.snapshot.targetMs, 60)
        for _ in 0..<3 { ring.append(block(0.1)) }
        _ = render(ring, frames: 2881, at: 6_000_000)
        XCTAssertEqual(ring.snapshot.targetMs, 120)
        XCTAssertEqual(ring.snapshot.underruns, 2)
        for _ in 0..<6 { ring.append(block(0.1)) }
        _ = render(ring, frames: 1, at: 35_999_999)
        XCTAssertEqual(ring.snapshot.targetMs, 120)
        _ = render(ring, frames: 1, at: 36_000_000)
        XCTAssertEqual(ring.snapshot.targetMs, 60)
        XCTAssertLessThanOrEqual(ring.snapshot.bufferMs, 80)
    }

    func testRenderedToneAndSilenceMetersUseActualOutput() {
        let ring = AudioPCMBuffer(meterEnabled: true)
        let tone = (0..<960).flatMap { frame -> [Float] in
            let value = Float(pow(10, -12.0 / 20) * sin(2 * Double.pi * 1_000 * Double(frame) / 48_000))
            return [value, value]
        }
        for _ in 0..<3 { ring.append(tone) }
        for i in 0..<10 {
            _ = render(ring, frames: 960, at: UInt64(i * 20_000))
            ring.append(tone)
        }
        XCTAssertEqual(ring.snapshot.tone1kDB, -12, accuracy: 0.1)
        XCTAssertEqual(ring.snapshot.rms, pow(10, -12.0 / 20) / sqrt(2), accuracy: 0.001)
        ring.reset()
        for _ in 0..<3 { ring.append(block(0)) }
        for i in 0..<10 {
            _ = render(ring, frames: 960, at: UInt64(200_000 + i * 20_000))
            ring.append(block(0))
        }
        XCTAssertEqual(ring.snapshot.rms, 0)
        XCTAssertEqual(ring.snapshot.tone1kDB, -120)
    }

    func testDtxSilenceDoesNotReplaySilenceAlreadyRendered() {
        let ring = AudioPCMBuffer()
        for _ in 0..<3 { ring.append(block(0.1)) }
        _ = render(ring, frames: 3840, at: 100_000) // 60 ms audio + 20 ms silence
        ring.appendSilence(frames: 1920) // a 40 ms DTX gap
        XCTAssertEqual(ring.snapshot.bufferMs, 20)
    }
}

final class AudioTimelineTests: XCTestCase {
    private func packet(_ seq: UInt32, epoch: UInt32 = 1, timestamp: UInt64? = nil,
                        arrival: UInt64 = 0, discontinuity: Bool = false) throws -> ReceivedAudio {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "tone_1khz_20ms",
            withExtension: "opus", subdirectory: "Fixtures"))
        return ReceivedAudio(header: AudioHeader(sessionId: 7, streamEpoch: epoch, audioSeq: seq,
            captureTimestampUs: timestamp ?? UInt64(seq) * 20_000, discontinuity: discontinuity),
            opus: try Data(contentsOf: url), receivedUs: arrival)
    }

    func testReorderingWithinDeadlineAvoidsConcealment() throws {
        let timeline = try AudioTimeline(pcm: AudioPCMBuffer())
        try timeline.receive(packet(1, discontinuity: true)); try timeline.drain(nowUs: 0)
        try timeline.receive(packet(3, arrival: 20_000)); try timeline.drain(nowUs: 30_000)
        XCTAssertEqual(timeline.decoded, 1)
        try timeline.receive(packet(2, arrival: 35_000)); try timeline.drain(nowUs: 35_000)
        XCTAssertEqual(timeline.decoded, 3)
        XCTAssertEqual(timeline.lost, 0)
        XCTAssertEqual(timeline.pcm.snapshot.bufferMs, 60)
        try timeline.receive(packet(2)); try timeline.drain(nowUs: 40_000)
        XCTAssertEqual(timeline.decoded, 3)
    }

    func testGapConcealsOnceAndRejectsLateRepair() throws {
        let timeline = try AudioTimeline(pcm: AudioPCMBuffer())
        try timeline.receive(packet(1)); try timeline.drain(nowUs: 0)
        try timeline.receive(packet(3, arrival: 20_000)); try timeline.drain(nowUs: 40_000)
        XCTAssertEqual(timeline.decoded, 2)
        XCTAssertEqual(timeline.lost, 1)
        XCTAssertEqual(timeline.concealed, 1)
        XCTAssertEqual(timeline.pcm.snapshot.bufferMs, 60)
        try timeline.receive(packet(2)); try timeline.drain(nowUs: 60_000)
        XCTAssertEqual(timeline.lost, 1)
        XCTAssertEqual(timeline.decoded, 2)
    }

    func testDtxGapAndCodecDiscontinuityKeepTheClockWithoutLoss() throws {
        let timeline = try AudioTimeline(pcm: AudioPCMBuffer())
        try timeline.receive(packet(1)); try timeline.drain(nowUs: 0)
        try timeline.receive(packet(2, timestamp: 80_000, discontinuity: true))
        try timeline.drain(nowUs: 60_000)
        XCTAssertEqual(timeline.lost, 0)
        XCTAssertEqual(timeline.decoded, 2)
        XCTAssertEqual(timeline.pcm.snapshot.bufferMs, 80, "20 audio + 40 silence + 20 audio")
    }

    func testEpochChangeClearsOldAudioAndRejectsPriorEpoch() throws {
        let timeline = try AudioTimeline(pcm: AudioPCMBuffer())
        try timeline.receive(packet(1)); try timeline.receive(packet(2)); try timeline.drain(nowUs: 0)
        try timeline.receive(packet(1, epoch: 2, discontinuity: true)); try timeline.drain(nowUs: 0)
        XCTAssertEqual(timeline.pcm.snapshot.bufferMs, 20)
        try timeline.receive(packet(3, epoch: 1)); try timeline.drain(nowUs: 50_000)
        XCTAssertEqual(timeline.decoded, 3)
        XCTAssertEqual(timeline.stale, 1)
    }

    func testSameEpochRestartAndSequenceWrapAreAccepted() throws {
        let timeline = try AudioTimeline(pcm: AudioPCMBuffer())
        try timeline.receive(packet(UInt32.max, timestamp: 20_000)); try timeline.drain(nowUs: 0)
        try timeline.receive(packet(0, timestamp: 40_000)); try timeline.drain(nowUs: 20_000)
        XCTAssertEqual(timeline.lost, 0)
        try timeline.receive(packet(1, timestamp: 60_000)); try timeline.drain(nowUs: 40_000)
        try timeline.receive(packet(1, timestamp: 160_000, discontinuity: true)); try timeline.drain(nowUs: 140_000)
        XCTAssertEqual(timeline.pcm.snapshot.bufferMs, 20)
        XCTAssertEqual(timeline.decoded, 4)
    }

    func testFarFutureFloodIsBoundedAndDoesNotRunUnboundedPlc() throws {
        let timeline = try AudioTimeline(pcm: AudioPCMBuffer())
        try timeline.receive(packet(1)); try timeline.drain(nowUs: 0)
        for seq in 1_000_000..<1_000_100 { try timeline.receive(packet(UInt32(seq))) }
        XCTAssertEqual(timeline.pendingCount, 8)
        try timeline.drain(nowUs: 50_000)
        XCTAssertEqual(timeline.lost, 999_998)
        XCTAssertEqual(timeline.concealed, 0)
        XCTAssertLessThanOrEqual(timeline.pcm.snapshot.bufferMs, 80)
    }

    func testMalformedPacketIsConcealedAndFollowingPacketStillDecodes() throws {
        let timeline = try AudioTimeline(pcm: AudioPCMBuffer())
        let valid = try packet(1)
        let malformed = ReceivedAudio(header: valid.header, opus: Data([3]), receivedUs: 0)
        try timeline.receive(malformed); try timeline.receive(packet(2)); try timeline.drain(nowUs: 0)
        XCTAssertEqual(timeline.lost, 1)
        XCTAssertEqual(timeline.decoded, 1)
    }

    func testMediaDemuxOnlyForwardsAudioForAcceptedSession() throws {
        let demux = MediaDatagrams()
        var forwarded = 0
        demux.onAudioPacket = { _, _ in forwarded += 1 }
        let packet = try packet(1)
        let data = try XCTUnwrap(packet.header.encode(payload: packet.opus))
        demux.handle(data)
        XCTAssertEqual(forwarded, 0)
        demux.setAcceptedSessionId(8); demux.handle(data)
        XCTAssertEqual(forwarded, 0)
        demux.setAcceptedSessionId(7); demux.handle(data)
        XCTAssertEqual(forwarded, 1)
        demux.handle(Data(data.dropLast()))
        XCTAssertEqual(forwarded, 1)
        demux.reset(); demux.handle(data)
        XCTAssertEqual(forwarded, 1)
    }
}
