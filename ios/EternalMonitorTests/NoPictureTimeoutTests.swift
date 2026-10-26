import XCTest
@testable import EternalMonitor

/// A PC that answers but sends no picture before the connect timeout, as
/// PCs on 0.3.1 and 0.3.2 do while their second screen starts.
@MainActor
final class NoPictureTimeoutTests: XCTestCase {
    private let fast = ConnectionManager.Timing(connectTimeoutSeconds: 0.5, reconnectGraceSeconds: 0.1,
        reconnectBackoffSeconds: { _ in 0.05 })

    override func setUp() async throws {
        let previous = UserDefaults.standard.object(forKey: "autoReconnect")
        UserDefaults.standard.set(true, forKey: "autoReconnect")
        addTeardownBlock { UserDefaults.standard.set(previous, forKey: "autoReconnect") }
    }

    private func until(_ description: String, seconds: Double = 10, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    /// One manual attempt at `host` that it answers, ending in its timeout.
    private func timedOutAttempt(_ manager: ConnectionManager, at host: FakeUDPHost) async throws {
        manager.connect(host: "127.0.0.1", port: host.port)
        XCTAssertEqual(manager.state, .connecting)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        try await until("timed out") { manager.state == .disconnected }
    }

    /// A session on `host` that streams.
    private func streaming(_ host: FakeUDPHost) async throws -> ConnectionManager {
        let manager = ConnectionManager(timing: ConnectionManager.Timing(connectTimeoutSeconds: 1,
            reconnectGraceSeconds: 0.1, reconnectBackoffSeconds: { _ in 0.05 }))
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        try await until("handshake") { manager.hostInfo != nil }
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming") { manager.state == .connected }
        return manager
    }

    private func logged(_ manager: ConnectionManager, _ prefix: String) -> String? {
        manager.diagnostics.first { $0.message.hasPrefix(prefix) }?.message
    }

    /// The app gave up, not the user: the PC hears the error reason, and the
    /// support log names the session and how long the attempt waited.
    func testATimeoutSaysGoodbyeWithTheErrorReason() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: fast)
        defer { manager.cancel() }
        try await timedOutAttempt(manager, at: host)
        let reasons = await Task.detached { host.goodbyes() }.value
        XCTAssertEqual(reasons, Array(repeating: .error, count: ControlChannel.byeCopies))
        XCTAssertEqual(manager.problem, .noPicture)
        let timeout = logged(manager, "Session accepted by 127.0.0.1:") ?? ""
        XCTAssertTrue(timeout.contains("no video datagrams arrived. session_id=41 ms_since_start="), timeout)
        XCTAssertTrue(timeout.contains(" ms_since_accept="), timeout)
        XCTAssertNotNil(logged(manager, "Disconnect state=connecting reason=error"))
        XCTAssertNil(logged(manager, "Disconnect state=connecting reason=userDisconnect"))
    }

    func testTheFirstFrameLineNamesTheSessionAndTheWait() async throws {
        let host = FakeUDPHost()
        let manager = try await streaming(host)
        defer { manager.cancel() }
        let line = logged(manager, "First frame decoded successfully") ?? ""
        XCTAssertTrue(line.contains(" session_id=41 ms_since_start="), line)
        XCTAssertTrue(line.contains(" ms_since_accept="), line)
    }

    /// The PC restarts its stream and then answers every attempt without a
    /// picture: the cycle stops at the limit and leaves Try again up. Try
    /// again starts a new count, so its own timeout stops nothing.
    func testRepeatedNoPictureTimeoutsStopReconnectingUntilTryAgain() async throws {
        let host = FakeUDPHost()
        let manager = try await streaming(host)
        defer { manager.cancel() }
        host.sendBye(.error)
        try await until("reconnecting") { manager.reconnectAttempt == 1 }
        let answered = await Task.detached { (0..<NoPictureStreak.limit).filter { _ in host.acceptSession() }.count }.value
        XCTAssertEqual(answered, NoPictureStreak.limit)
        try await until("stopped") { manager.reconnectAttempt == 0 && manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .noPicture)
        XCTAssertEqual(manager.problem?.actions, [.retry])
        XCTAssertNotNil(logged(manager, "Stopped reconnecting after 2 no-picture timeouts in a row"))
        let more = await Task.detached { host.helloAttempts(within: 1.5) }.value
        XCTAssertEqual(more, 0, "no attempt after the limit")
        XCTAssertEqual(manager.problem, .noPicture, "the card stays up")

        manager.retry()
        XCTAssertEqual(manager.state, .connecting)
        XCTAssertNil(manager.problem)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "Try again never reached the PC")
        try await until("Try again times out") { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .noPicture)
        XCTAssertNil(logged(manager, "Stopped reconnecting"), "Try again starts a new count")
    }

    /// A PC that is slow only once still gets the next attempt, and streams.
    func testOneNoPictureTimeoutKeepsReconnecting() async throws {
        let host = FakeUDPHost()
        let manager = try await streaming(host)
        defer { manager.cancel() }
        host.sendBye(.error)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted)
        try await until("the first attempt times out") { manager.state == .disconnected && manager.problem == .noPicture }
        XCTAssertGreaterThan(manager.reconnectAttempt, 0, "the cycle goes on")
        let again = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(again, "the cycle never tried again")
        try await until("handshake") { manager.hostInfo != nil }
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming again") { manager.state == .connected }
        XCTAssertNil(manager.problem)
        XCTAssertEqual(manager.reconnectAttempt, 0)
    }

    /// The PC answered, then stops answering while EternalMonitor restarts
    /// on it: the card keeps the advice about the picture. Another PC that
    /// never answered gets its own message.
    func testAPCThatAnsweredBeforeKeepsTheNoPictureAdvice() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: fast)
        defer { manager.cancel() }
        try await timedOutAttempt(manager, at: host)
        XCTAssertEqual(manager.problem, .noPicture)

        manager.connect(host: "127.0.0.1", port: host.port)  // nothing answers now
        try await until("silent attempt times out") { manager.state == .disconnected }
        XCTAssertNotNil(logged(manager, "HELLO sent"), "the PC really was silent")
        XCTAssertEqual(manager.problem, .noPicture)

        let other = FakeUDPHost()
        manager.connect(host: "127.0.0.1", port: other.port)
        try await until("the other PC times out") { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .noReply(usb: false))
    }
}
