import XCTest
@testable import EternalMonitor

/// A PC that answers but isn't streaming (Stop clicked, restarting, or given
/// up after errors) says so. The iPad waits and keeps asking slowly, without
/// spending reconnect attempts, and connects as soon as the PC streams.
@MainActor
final class PCNotStreamingTests: XCTestCase {
    private func until(_ description: String, seconds: Double = 10, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    /// Longer than the slow HELLO2 retry, so a waiting attempt never times out
    /// between two answers.
    private let timing = ConnectionManager.Timing(
        connectTimeoutSeconds: 3, reconnectGraceSeconds: 0.1, reconnectBackoffSeconds: { _ in 0.05 })

    func testAStoppedPCIsWaitedForAndConnectsOnceItStarts() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: timing)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let answered = await Task.detached { host.acceptSession(status: .stopped) }.value
        XCTAssertTrue(answered, "the iPad never sent HELLO2")
        let advertised = (host.lastHello?.featureCaps ?? 0) & Hello2.featureStandbyStatus
        XCTAssertNotEqual(advertised, 0, "the PC must know this iPad understands the answer")
        try await until("told the PC isn't streaming") { manager.problem == .pcNotStreaming(.stopped) }
        XCTAssertEqual(manager.problem?.title, "Your PC isn't streaming")
        XCTAssertEqual(manager.problem?.detail,
            "Click Start streaming in EternalMonitor on the PC. This iPad connects as soon as it starts.")
        XCTAssertEqual(manager.state, .connecting)
        XCTAssertEqual(manager.connectingStatus, .waitingForStream)

        // Still waiting past the connect timeout while the PC keeps answering.
        for _ in 0..<2 {
            let again = await Task.detached { host.acceptSession(status: .stopped) }.value
            XCTAssertTrue(again, "the iPad stopped asking")
        }
        XCTAssertEqual(manager.state, .connecting)

        // Start on the PC: the same attempt connects.
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted)
        try await until("handshake") { manager.hostInfo != nil }
        XCTAssertNil(manager.problem)
        XCTAssertEqual(manager.connectingStatus, .answered)
    }

    func testAFailedPCSaysWhyInItsOwnWords() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: timing)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let answered = await Task.detached {
            host.acceptSession(status: .failed, reason: "Windows is changing the display")
        }.value
        XCTAssertTrue(answered)
        try await until("told why") { manager.problem == .pcNotStreaming(.failed("Windows is changing the display")) }
        XCTAssertEqual(manager.problem?.title, "Your PC couldn't start streaming")
        XCTAssertEqual(manager.problem?.detail, "Windows is changing the display. Click Start streaming in "
            + "EternalMonitor on the PC to try again. This iPad connects as soon as it starts.")
        XCTAssertEqual(manager.problem?.actions, [])
    }

    /// The PC was stopped, then closed: once it stops answering, the iPad
    /// says it didn't answer, not that no picture arrived.
    func testAPCThatStopsAnsweringTimesOutAsNoReply() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: timing)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let answered = await Task.detached { host.acceptSession(status: .stopped) }.value
        XCTAssertTrue(answered)
        try await until("timed out", seconds: 8) { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .noReply(usb: false))
    }

    /// A restarting PC doesn't run the reconnect cycle out: the cycle's
    /// attempt waits, and connects when the stream is back.
    func testARestartingPCDoesNotSpendReconnectAttempts() async throws {
        let previous = UserDefaults.standard.object(forKey: "autoReconnect")
        UserDefaults.standard.set(true, forKey: "autoReconnect")
        defer { UserDefaults.standard.set(previous, forKey: "autoReconnect") }
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: timing)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let first = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(first)
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming") { manager.state == .connected }

        // The PC's stream fails and it restarts.
        host.sendBye(.error)
        try await until("reconnecting") { manager.reconnectAttempt == 1 }
        for _ in 0..<4 {
            let answered = await Task.detached { host.acceptSession(status: .restarting) }.value
            XCTAssertTrue(answered, "the iPad stopped asking")
        }
        XCTAssertEqual(manager.problem, .pcNotStreaming(.restarting))
        XCTAssertEqual(manager.problem?.title, "Your PC is restarting the stream…")
        XCTAssertEqual(manager.reconnectAttempt, 1, "waiting must not spend attempts")

        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted)
        try await until("handshake") { manager.hostInfo != nil }
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming again") { manager.state == .connected }
        XCTAssertNil(manager.problem)
        XCTAssertEqual(manager.reconnectAttempt, 0)
    }

    func testCancelWhileWaitingClearsTheMessage() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: timing)
        manager.connect(host: "127.0.0.1", port: host.port)
        let answered = await Task.detached { host.acceptSession(status: .stopped) }.value
        XCTAssertTrue(answered)
        try await until("waiting") { manager.problem == .pcNotStreaming(.stopped) }
        manager.cancel()
        XCTAssertEqual(manager.state, .disconnected)
        XCTAssertNil(manager.problem)
    }
}
