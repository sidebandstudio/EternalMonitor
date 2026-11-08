import Combine
import XCTest
@testable import EternalMonitor

/// A PC that answers but sends no picture before the connect timeout, as
/// PCs on 0.3.1 and 0.3.2 do while their second screen starts.
@MainActor
final class NoPictureTimeoutTests: XCTestCase {
    /// The timeout under test: attempts that end without a picture end quickly.
    private let fast = ConnectionManager.Timing(connectTimeoutSeconds: 0.5, reconnectGraceSeconds: 0.1,
        reconnectBackoffSeconds: { _ in 0.05 })
    /// The wait for the timeout under test: its 0.5 s on a main actor
    /// stalled for seconds, and short of a watchdog that takes 5 s.
    nonisolated private static let timeoutWaitSeconds = 4.0
    /// For an attempt that has to stream, and the waits for it: long enough
    /// that the fake PC's handshake and picture never run out on a loaded
    /// machine. A test of the timeout shortens it once the session streams.
    private static let streamingTimeoutSeconds = 60.0

    override func setUp() async throws {
        let previous = UserDefaults.standard.object(forKey: "autoReconnect")
        UserDefaults.standard.set(true, forKey: "autoReconnect")
        addTeardownBlock { UserDefaults.standard.set(previous, forKey: "autoReconnect") }
    }

    private func until(_ description: String, seconds: Double = NoPictureTimeoutTests.timeoutWaitSeconds, _ condition: () -> Bool) async throws {
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
        var timing = fast
        timing.connectTimeoutSeconds = Self.streamingTimeoutSeconds
        let manager = ConnectionManager(timing: timing)
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        try await until("handshake", seconds: Self.streamingTimeoutSeconds) { manager.hostInfo != nil }
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming", seconds: Self.streamingTimeoutSeconds) { manager.state == .connected }
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
        XCTAssertNotNil(logged(manager, "Disconnect state=connecting reason=connectTimeout"))
        XCTAssertNil(logged(manager, "Disconnect state=connecting reason=userDisconnect"))
    }

    /// The hybrid laptops: the PC accepted, then its heartbeats said
    /// Windows won't let it capture from its graphics card. The iPad showed
    /// "Connecting…" for 10 s, then "no picture". Now it says why while it
    /// waits, its card says what to do on the PC when the wait runs out, and
    /// its summary names a capture failure, as the PC's does.
    func testAPCThatCantCaptureFromItsGraphicsCardSaysWhyInsteadOfNoPicture() async throws {
        let host = FakeUDPHost()
        var timing = fast
        timing.connectTimeoutSeconds = 3
        let manager = ConnectionManager(timing: timing)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        host.sendHeartbeat(notice: Heartbeat.noticeUnsupportedGPU)
        try await until("the PC says why") { manager.pcGraphicsCard == .screen }
        XCTAssertEqual(manager.state, .connecting, "still waiting for a picture")
        try await until("timed out", seconds: 15) { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .pcGraphicsCard(.screen))
        XCTAssertEqual(manager.problem?.actions, [.retry])
        let summary = logged(manager, "session_summary v=1 role=ipad") ?? ""
        XCTAssertTrue(summary.contains(" stage=accepted outcome=no_picture failure=capture_failed "), summary)
        XCTAssertNil(manager.pcGraphicsCard, "the next attempt starts without it")
    }

    /// The PC counted video it sent before Windows refused its card, and no
    /// picture came: the card still says why, and the summary keeps the
    /// lost video, as the PC's does.
    func testVideoSentBeforeTheRefusalIsStillCountedAsLost() async throws {
        let host = FakeUDPHost()
        var timing = fast
        timing.connectTimeoutSeconds = 3
        let manager = ConnectionManager(timing: timing)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        host.sendHeartbeat(framesSent: 5)
        host.sendHeartbeat(notice: Heartbeat.noticeUnsupportedGPU, framesSent: 5)
        try await until("the PC says why") { manager.pcGraphicsCard == .screen }
        try await until("timed out", seconds: 15) { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .pcGraphicsCard(.screen))
        let summary = logged(manager, "session_summary v=1 role=ipad") ?? ""
        XCTAssertTrue(summary.contains(" outcome=no_picture failure=no_video "), summary)
    }

    /// The same in Second screen mode, after the second screen failed.
    func testThePCsSecondScreenModeKeepsItsOwnCard() async throws {
        let host = FakeUDPHost()
        var timing = fast
        timing.connectTimeoutSeconds = 3
        let manager = ConnectionManager(timing: timing)
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        host.sendHeartbeat(notice: Heartbeat.noticeSecondScreenUnsupportedGPU)
        try await until("the PC says why") { manager.pcGraphicsCard == .secondScreen }
        try await until("timed out", seconds: 15) { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .pcGraphicsCard(.secondScreen))
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
        manager.timing.connectTimeoutSeconds = fast.connectTimeoutSeconds
        // The cycle's attempt numbers as they change: a poll can miss the
        // first on a loaded machine once the cycle has moved on.
        var attempts: [Int] = []
        let cycle = manager.$reconnectAttempt.sink { attempts.append($0) }
        defer { cycle.cancel() }
        host.sendBye(.error)
        let answered = await Task.detached { (0..<NoPictureStreak.limit).filter { _ in host.acceptSession() }.count }.value
        XCTAssertTrue(attempts.contains(1), "reconnecting")
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
        // Only the cycle's first attempt has the short timeout. As the cycle
        // moves on to the second, which has to stream, it gets the long one
        // back, and what the first left is read then: polling can miss the
        // moment between the two on a loaded machine.
        manager.timing.connectTimeoutSeconds = fast.connectTimeoutSeconds
        var leftByTheFirst: (state: ConnectionState, problem: ConnectionProblem?)?
        let second = manager.$reconnectAttempt.sink { attempt in
            guard attempt == 2 else { return }
            manager.timing.connectTimeoutSeconds = Self.streamingTimeoutSeconds
            leftByTheFirst = (manager.state, manager.problem)
        }
        defer { second.cancel() }
        host.sendBye(.error)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted)
        let answered = Date()
        let again = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(again, "the cycle never tried again")
        let untilTheNextAttempt = Date().timeIntervalSince(answered)
        XCTAssertLessThan(untilTheNextAttempt, Self.timeoutWaitSeconds, "the first attempt took \(untilTheNextAttempt) s to time out")
        XCTAssertNotNil(leftByTheFirst, "the cycle goes on")
        XCTAssertEqual(leftByTheFirst?.state, .disconnected, "the first attempt times out")
        XCTAssertEqual(leftByTheFirst?.problem, .noPicture, "the first attempt times out")
        try await until("handshake", seconds: Self.streamingTimeoutSeconds) { manager.hostInfo != nil }
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming again", seconds: Self.streamingTimeoutSeconds) { manager.state == .connected }
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

    /// The support log's "no datagrams came back" line is a warning at a PC
    /// that never answered, and an error once that PC has answered here.
    func testAnUnansweredHelloIsAWarningUntilThatPCAnswered() async throws {
        UserDefaults.standard.set(false, forKey: "autoReconnect")
        let host = FakeUDPHost()
        let manager = ConnectionManager(timing: fast)
        defer { manager.cancel() }
        func unansweredLevel() async throws -> DiagnosticLevel? {
            manager.connect(host: "127.0.0.1", port: host.port)
            try await until("timed out") { manager.state == .disconnected }
            _ = await Task.detached { host.goodbyes() }.value // drain the unanswered HELLO2s
            return manager.diagnostics.first { $0.message.contains("but no datagrams came back") }?.level
        }
        let before = try await unansweredLevel()
        XCTAssertEqual(before, .warning)
        try await timedOutAttempt(manager, at: host)
        _ = await Task.detached { host.goodbyes() }.value
        let after = try await unansweredLevel()
        XCTAssertEqual(after, .error)
    }
}
