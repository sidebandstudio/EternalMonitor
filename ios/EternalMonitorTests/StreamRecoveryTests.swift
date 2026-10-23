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

    func testContiguousNumberingRevealsALostReopenKeyframe() async throws {
        // The PC reopened its encoder and its new keyframe (seq 2) was lost;
        // the new encoder's first P-frame continues the codec's count. A PC
        // advertising contiguous numbering lets the gap show it.
        let units = try twoGOPs()
        let numbered = FakeUDPHost()
        _ = try await streaming(numbered, hostCaps: ReferenceContinuity.hostCapContiguousFrameSeq)
        numbered.sendFrame(units[5], seq: 3, isKeyframe: false)
        let asked = await Task.detached { numbered.keyframeRequests(within: 1) }.value
        XCTAssertEqual(asked.first, .decodeError)

        let captured = FakeUDPHost()
        _ = try await streaming(captured)
        captured.sendFrame(units[5], seq: 3, isKeyframe: false)
        let quiet = await Task.detached { captured.keyframeRequests(within: 1) }.value
        XCTAssertEqual(quiet, [], "a 0.3.1 PC's numbers skip on their own")
    }
}
