import XCTest
@testable import EternalMonitor

/// Recovery as the PC sees it: what the iPad asks for, through the real
/// connection, reassembler and decoder.
@MainActor
final class StreamRecoveryTests: XCTestCase {
    /// IDR P P P IDR P P P, as the encoder sends them.
    private func twoGOPs() throws -> [Data] {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "gop2_64x64", withExtension: "h264", subdirectory: "Fixtures"))
        let data = try Data(contentsOf: url)
        var starts: [(Int, Int)] = []
        var i = 0
        while i + 3 <= data.count {
            if data[i] == 0, data[i + 1] == 0, data[i + 2] == 1 { starts.append((i > 0 && data[i - 1] == 0 ? i - 1 : i, i + 3)); i += 3 } else { i += 1 }
        }
        var units: [Data] = []
        var pending = Data()
        for (n, start) in starts.enumerated() {
            pending.append(data[start.0..<(n + 1 < starts.count ? starts[n + 1].0 : data.count)])
            if [1, 5].contains(data[start.1] & 0x1F) { units.append(pending); pending = Data() }
        }
        return units
    }

    private func streaming(_ host: FakeUDPHost, hostCaps: UInt16 = 0) async throws -> ConnectionManager {
        let manager = ConnectionManager()
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession(hostCaps: hostCaps) }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        let units = try twoGOPs()
        host.sendFrame(units[0], seq: 1, isKeyframe: true)
        for _ in 0..<300 where manager.state != .connected { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(manager.state, .connected)
        addTeardownBlock { await MainActor.run { manager.cancel() } }
        return manager
    }

    func testFrozenPictureAsksThePCToRestartTheStream() async throws {
        // A frame is lost and nothing more arrives: after 2 s of frozen
        // picture the iPad sends Resume, which a PC with this fix answers by
        // reopening its encoder (0.3.1 PCs force a keyframe).
        let host = FakeUDPHost()
        _ = try await streaming(host)
        let units = try twoGOPs()
        host.sendFrame(units[1], seq: 2, isKeyframe: false)
        host.sendFrame(units[3], seq: 4, isKeyframe: false)
        let reasons = await Task.detached { host.keyframeRequests(within: 3.2) }.value
        XCTAssertEqual(reasons.first, .decodeError, "the hold asks for a keyframe at once")
        // Then every 600 ms; from 2 s each retry asks for a restart instead.
        let firstResume = try XCTUnwrap(reasons.firstIndex(of: .resume), "\(reasons)")
        XCTAssertGreaterThanOrEqual(firstResume, 3, "\(reasons)")
        XCTAssertTrue(reasons[..<firstResume].allSatisfy { $0 == .decodeError }, "\(reasons)")
    }

    func testContiguousNumberingRevealsALostReopenKeyframe() async throws {
        // The PC reopened its encoder and its new keyframe (seq 2) was lost;
        // the new encoder's first P-frame continues the codec's count. A PC
        // advertising contiguous numbering lets the gap show it.
        let units = try twoGOPs()
        let numbered = FakeUDPHost()
        _ = try await streaming(numbered, hostCaps: HelloAck.hostCapContiguousFrameSeq)
        numbered.sendFrame(units[5], seq: 3, isKeyframe: false)
        let asked = await Task.detached { numbered.keyframeRequests(within: 1) }.value
        XCTAssertEqual(asked.first, .decodeError)

        let captured = FakeUDPHost()
        _ = try await streaming(captured)
        captured.sendFrame(units[5], seq: 3, isKeyframe: false)
        let quiet = await Task.detached { captured.keyframeRequests(within: 1) }.value
        XCTAssertEqual(quiet, [], "a 0.3.1 PC's numbers skip on their own")
    }

    // MARK: - A lost frame through the reassembler

    /// Sends `frame` as frames `seqs`, 100 ms apart, off the main actor.
    private func stream(_ host: FakeUDPHost, _ frame: Data, seqs: ClosedRange<UInt32>) -> Task<Void, Never> {
        Task.detached {
            for seq in seqs {
                host.sendFrame(frame, seq: seq, isKeyframe: false)
                try? await Task.sleep(nanoseconds: 100_000_000)
            }
        }
    }

    private func until(_ description: String, seconds: Double = 5, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    /// Requests about 600 ms apart: past the PC's 500 ms limit, well inside a second.
    private func assertRetrySpacing(_ requests: [(reason: KeyframeReason, at: Date)], file: StaticString = #filePath,
                                    line: UInt = #line) {
        XCTAssertGreaterThanOrEqual(requests.count, 2, "\(requests.map(\.reason))", file: file, line: line)
        for (earlier, later) in zip(requests, requests.dropFirst()) {
            let gap = later.at.timeIntervalSince(earlier.at)
            XCTAssertTrue((0.45...1.1).contains(gap), "retry \(gap) s after the last", file: file, line: line)
        }
    }

    /// One P-frame loses a fragment and the reassembler drops it (without
    /// repair, 100 ms after its latest fragment). The frames after it would
    /// decode against a missing reference (-12909, smeared blocks): none is
    /// shown, the last picture stays, keyframe requests repeat until the next
    /// IDR, and that IDR ends the hold. A 0.3.4 PC's contiguous numbering
    /// and a 0.3.3 PC's H.264 frame_num both reveal the loss.
    func testALostPFrameHoldsThePictureAndAsksUntilTheNextIDR() async throws {
        for hostCaps in [HelloAck.hostCapContiguousFrameSeq, 0] {
            let units = try twoGOPs()
            let host = FakeUDPHost()
            let manager = try await streaming(host, hostCaps: hostCaps)
            host.sendFrame(units[1], seq: 2, isKeyframe: false)
            try await until("the P-frame before the loss is shown") { manager.debugState.decodedFrames == 2 }

            host.sendFirstFragment(units[2], seq: 3)
            let following = stream(host, units[3], seqs: 4...20)
            let held = await Task.detached { host.timedKeyframeRequests(within: 1.9) }.value
            XCTAssertEqual(manager.debugState.decodedFrames, 2, "caps \(hostCaps): no frame after the loss is shown")
            XCTAssertNotNil(manager.videoDecoder?.keyframeWaitUs, "caps \(hostCaps): the last picture is held")
            XCTAssertTrue(manager.pictureRestoring, "caps \(hostCaps): Restoring the picture after 1 s")
            // The reassembler asks once as it drops the frame; the held decoder asks after.
            XCTAssertEqual(held.first?.reason, .gapLoss, "caps \(hostCaps): \(held.map(\.reason))")
            XCTAssertTrue(held.dropFirst().allSatisfy { $0.reason == .decodeError }, "caps \(hostCaps): \(held.map(\.reason))")
            XCTAssertGreaterThanOrEqual(held.count, 3, "caps \(hostCaps): asked at once, then again")
            assertRetrySpacing(held)
            await following.value

            host.sendFrame(units[4], seq: 21, isKeyframe: true)
            let resumed = stream(host, units[5], seqs: 22...22)
            await resumed.value
            try await until("caps \(hostCaps): the IDR and the frame after it are shown") {
                manager.debugState.decodedFrames == 4
            }
            let after = await Task.detached { host.keyframeRequests(within: 1.2) }.value
            XCTAssertEqual(after, [], "caps \(hostCaps): the IDR ends the requests")
            XCTAssertNil(manager.videoDecoder?.keyframeWaitUs)
            try await until("caps \(hostCaps): the note goes") { !manager.pictureRestoring }
            XCTAssertFalse(manager.diagnostics.contains { $0.category == "decode" && $0.message.contains("-12909") },
                           "caps \(hostCaps): nothing was decoded against the missing frame")
            manager.cancel()
        }
    }

    /// The session's first IDR is lost: the P-frames that arrive instead
    /// are not decoded, and each 600 ms one of them asks again until an IDR
    /// comes and the picture starts.
    func testALostFirstIDRIsAskedForUntilOneArrives() async throws {
        let units = try twoGOPs()
        let host = FakeUDPHost()
        let manager = ConnectionManager()
        addTeardownBlock { await MainActor.run { manager.cancel() } }
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession(hostCaps: HelloAck.hostCapContiguousFrameSeq) }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")

        let dependents = stream(host, units[1], seqs: 2...16)
        let asked = await Task.detached { host.timedKeyframeRequests(within: 1.6) }.value
        XCTAssertEqual(manager.debugState.decodedFrames, 0)
        XCTAssertEqual(manager.state, .connecting)
        XCTAssertTrue(asked.allSatisfy { $0.reason == .decodeError }, "\(asked.map(\.reason))")
        assertRetrySpacing(asked)
        await dependents.value

        host.sendFrame(units[4], seq: 17, isKeyframe: true)
        try await until("the IDR starts the picture") { manager.state == .connected }
        XCTAssertEqual(manager.debugState.decodedFrames, 1)
        let after = await Task.detached { host.keyframeRequests(within: 1) }.value
        XCTAssertEqual(after, [])
    }
}
