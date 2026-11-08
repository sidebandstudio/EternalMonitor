import XCTest
@testable import EternalMonitor

/// A PC that answers but isn't streaming (Stop clicked, restarting, or
/// stopped after repeated errors) says so. The iPad shows why, keeps asking
/// slowly without spending reconnect attempts, and connects as soon as the
/// PC streams again.
@MainActor
final class PCNotStreamingTests: XCTestCase {
    private func until(_ description: String, seconds: Double = 30, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    /// Longer than the slow HELLO2 retry, so a waiting attempt never times
    /// out between two answers, even on a loaded machine.
    private let timing = ConnectionManager.Timing(
        connectTimeoutSeconds: 5, reconnectGraceSeconds: 0.1, reconnectBackoffSeconds: { _ in 0.05 })

    func testAStoppedPCIsWaitedForAndConnectsOnceItStarts() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: timing)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let answered = await Task.detached { host.acceptSession(status: .stopped) }.value
        XCTAssertTrue(answered, "the iPad never sent HELLO2")
        let advertised = (host.lastHello?.featureCaps ?? 0) & Hello2.featureStandbyStatus
        XCTAssertNotEqual(advertised, 0, "the PC must know this iPad understands the answer")
        XCTAssertGreaterThanOrEqual(host.lastHelloSize, Hello2.standbyLength, "padded so any answer fits")
        let firstNonce = host.lastNonce
        try await until("told the PC isn't streaming") { manager.problem == .pcNotStreaming(.stopped) }
        XCTAssertEqual(manager.problem?.title, "Your PC isn't streaming")
        XCTAssertEqual(manager.problem?.detail,
            "Click Start streaming in EternalMonitor on the PC. This iPad connects as soon as it starts.")
        XCTAssertEqual(manager.state, .connecting)

        // It keeps asking, every two seconds rather than four times a
        // second, with the same HELLO2, past the connect timeout. The gaps
        // are between the HELLO2s the PC received, by their arrival times,
        // from those sent after the iPad showed it heard the answer: a quick
        // retry sent before that may still have been on its way.
        let told = Date()
        var askedAt: [Date] = []
        for _ in 0..<6 where askedAt.count < 3 {
            let again = await Task.detached { host.acceptSession(status: .stopped) }.value
            XCTAssertTrue(again, "the iPad stopped asking")
            XCTAssertEqual(host.lastNonce, firstNonce)
            let arrival = try XCTUnwrap(host.lastHelloArrival, "no kernel receive time for the HELLO2")
            if arrival > told { askedAt.append(arrival) }
        }
        XCTAssertEqual(askedAt.count, 3, "the iPad stopped asking")
        for (earlier, later) in zip(askedAt.dropLast(), askedAt.dropFirst()) {
            XCTAssertGreaterThan(later.timeIntervalSince(earlier), 1.5, "asking too often")
        }
        XCTAssertEqual(manager.state, .connecting)
        XCTAssertEqual(manager.problem, .pcNotStreaming(.stopped))

        // Start on the PC: the same attempt connects.
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted)
        XCTAssertEqual(host.lastNonce, firstNonce)
        try await until("handshake") { manager.hostInfo != nil }
        XCTAssertNil(manager.problem)
    }

    func testAFailedPCSaysWhyInWords() async throws {
        for (token, detail) in [
            ("display_changing", "Windows is changing the display. Click Start streaming in EternalMonitor on the PC "
                + "to try again. This iPad connects as soon as it starts."),
            ("a_reason_from_a_newer_pc", "Click Start streaming in EternalMonitor on the PC to try again. "
                + "This iPad connects as soon as it starts.")
        ] {
            let host = FakeUDPHost()
            let manager = ConnectionManager(timing: timing)
            manager.connect(host: "127.0.0.1", port: host.port)
            let answered = await Task.detached { host.acceptSession(status: .failed, reason: token) }.value
            XCTAssertTrue(answered)
            try await until("told why") { manager.problem == .pcNotStreaming(.failed(token)) }
            XCTAssertEqual(manager.problem?.title, "Your PC stopped streaming after repeated errors")
            XCTAssertEqual(manager.problem?.detail, detail)
            XCTAssertEqual(manager.problem?.actions, [])
            XCTAssertEqual(manager.state, .connecting)
            manager.cancel()
        }
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
        try await until("timed out") { manager.state == .disconnected }
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
        var answeredAt: [Date] = []
        for _ in 0..<4 {
            let answered = await Task.detached { host.acceptSession(status: .restarting) }.value
            XCTAssertTrue(answered, "the iPad stopped asking")
            answeredAt.append(Date())
        }
        for (earlier, later) in zip(answeredAt.dropLast(), answeredAt.dropFirst()) {
            XCTAssertGreaterThan(later.timeIntervalSince(earlier), 0.7, "asking too often")
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

    /// The attempt keeps its nonce while it waits, so a Restarting answer
    /// can arrive after the OK (Wi-Fi reordered it, or a late copy). Once the
    /// session is up, it changes nothing: no card, no new timeout.
    func testARestartingAnswerAfterTheSessionStartedIsIgnored() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: timing)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let waiting = await Task.detached { host.acceptSession(status: .restarting) }.value
        XCTAssertTrue(waiting)
        try await until("waiting") { manager.problem == .pcNotStreaming(.restarting) }
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted)
        try await until("handshake") { manager.hostInfo != nil }
        XCTAssertNil(manager.problem)

        host.sendAck(nonce: host.lastNonce, status: .restarting)
        try await Task.sleep(nanoseconds: 500_000_000)
        XCTAssertNil(manager.problem, "a late Restarting answer brought the card back")
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming") { manager.state == .connected }
        XCTAssertNil(manager.problem)
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
