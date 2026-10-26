import Darwin
import XCTest
@testable import EternalMonitor

/// Leaving a session must hand every BYE copy to the network before the link
/// is torn down, and that intentional teardown must not raise a problem.
@MainActor
final class GoodbyeTests: XCTestCase {
    private func connectedManager(_ host: FakeUDPHost, timing: ConnectionManager.Timing = .init()) async throws
        -> ConnectionManager {
        let manager = ConnectionManager(timing: timing)
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        for _ in 0..<200 where manager.hostInfo == nil {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertEqual(manager.hostInfo?.sessionId, 41)
        return manager
    }

    func testBackgroundingSendsEveryGoodbyeBeforeTheLinkCloses() async throws {
        let host = FakeUDPHost()
        let manager = try await connectedManager(host)
        defer { manager.cancel() }
        manager.handleAppBackgrounded()
        XCTAssertEqual(manager.state, .disconnected)
        let reasons = await Task.detached { host.goodbyes() }.value
        XCTAssertEqual(reasons.filter { $0 == .appBackground }.count, ControlChannel.byeCopies,
            "every BYE copy must leave before teardown, not on a timer the teardown cancels")
        // The intentional teardown raises no problem banner.
        XCTAssertNil(manager.problem)
        XCTAssertNil(manager.connectionError)
    }

    /// The CI lifecycle scenario's three Home/foreground cycles, without the UI:
    /// every cycle says goodbye in full and resumes.
    func testEveryBackgroundCycleSaysGoodbyeAndResumes() async throws {
        let previous = UserDefaults.standard.object(forKey: "autoResumeOnForeground")
        UserDefaults.standard.set(true, forKey: "autoResumeOnForeground")
        defer { UserDefaults.standard.set(previous, forKey: "autoResumeOnForeground") }
        let host = FakeUDPHost()
        let manager = try await connectedManager(host)
        defer { manager.cancel() }
        for cycle in 1...3 {
            manager.handleAppBackgrounded()
            let reasons = await Task.detached { host.goodbyes() }.value
            XCTAssertEqual(reasons.filter { $0 == .appBackground }.count, ControlChannel.byeCopies, "cycle \(cycle)")
            XCTAssertNil(manager.problem, "cycle \(cycle)")
            manager.handleAppForegrounded()
            XCTAssertEqual(manager.state, .connecting, "cycle \(cycle) must resume")
            let accepted = await Task.detached { host.acceptSession() }.value
            XCTAssertTrue(accepted, "cycle \(cycle) never sent HELLO2")
            for _ in 0..<200 where manager.hostInfo == nil {
                try await Task.sleep(nanoseconds: 10_000_000)
            }
            XCTAssertEqual(manager.hostInfo?.sessionId, 41, "cycle \(cycle)")
        }
    }

    func testDisconnectSendsEveryGoodbyeWithoutAProblem() async throws {
        let host = FakeUDPHost()
        let manager = try await connectedManager(host)
        manager.cancel()
        let reasons = await Task.detached { host.goodbyes() }.value
        XCTAssertEqual(reasons.filter { $0 == .userDisconnect }.count, ControlChannel.byeCopies)
        XCTAssertNil(manager.problem)
        XCTAssertNil(manager.connectionError)
    }

    // MARK: Goodbyes from the PC

    private func until(_ description: String, seconds: Double = 10, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    /// A streaming session on `host`, with automatic reconnecting on.
    private func streaming(_ host: FakeUDPHost) async throws -> ConnectionManager {
        let previous = UserDefaults.standard.object(forKey: "autoReconnect")
        UserDefaults.standard.set(true, forKey: "autoReconnect")
        addTeardownBlock { UserDefaults.standard.set(previous, forKey: "autoReconnect") }
        let manager = try await connectedManager(host, timing: ConnectionManager.Timing(
            connectTimeoutSeconds: 1, reconnectGraceSeconds: 0.1, reconnectBackoffSeconds: { _ in 0.05 }))
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming") { manager.state == .connected }
        return manager
    }

    /// The PC ends the stream to restart it, for a mode switch, new
    /// settings or an update, and comes back: the iPad reconnects by itself.
    func testAPCRestartingItsStreamIsReconnected() async throws {
        let host = FakeUDPHost()
        let manager = try await streaming(host)
        defer { manager.cancel() }
        host.sendBye(.hostShuttingDown)
        try await until("reconnecting") { manager.reconnectAttempt == 1 }
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never tried the PC again")
        try await until("handshake") { manager.hostInfo != nil }
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming again") { manager.state == .connected }
        XCTAssertNil(manager.problem)
        XCTAssertEqual(manager.reconnectAttempt, 0)
    }

    /// Stop and update on the PC: the iPad tries again while the PC turns it
    /// away for the update, and says the PC is updating, not that it is busy.
    func testAPCStoppingForAnUpdateSaysSo() async throws {
        let host = FakeUDPHost()
        let manager = try await streaming(host)
        defer { manager.cancel() }
        host.sendBye(.hostShuttingDown)
        let answered = await Task.detached { (0..<6).filter { _ in host.acceptSession(status: .busy) }.count }.value
        XCTAssertGreaterThan(answered, 0, "the iPad never tried the PC again")
        try await until("gave up", seconds: 20) { manager.reconnectAttempt == 0 && manager.state == .disconnected }
        // Every attempt ran out: no promise that it reconnects by itself.
        XCTAssertEqual(manager.problem?.title, "Your PC hasn't come back from its update yet")
        XCTAssertFalse(manager.problem?.detail.contains("reconnects") ?? true)
        XCTAssertEqual(manager.problem?.actions, [.retry])
    }

    /// With Auto-reconnect off nothing retries, so the message says to try
    /// again once the PC is back.
    func testAnUpdateWithAutoReconnectOffSaysToTryAgain() async throws {
        let host = FakeUDPHost()
        let manager = try await streaming(host)
        defer { manager.cancel() }
        UserDefaults.standard.set(false, forKey: "autoReconnect")
        host.sendBye(.hostShuttingDown)
        try await until("disconnected") { manager.state == .disconnected }
        XCTAssertEqual(manager.problem?.title, "Your PC is updating EternalMonitor")
        XCTAssertFalse(manager.problem?.detail.contains("reconnects") ?? true, manager.problem?.detail ?? "")
        XCTAssertEqual(manager.problem?.actions, [.retry])
        let attempts = await Task.detached { host.helloAttempts(within: 1.5) }.value
        XCTAssertEqual(attempts, 0)
    }

    /// The update popup waits for Connect to settle: never between reconnect
    /// attempts, where a tap meant for "Reconnecting" would hit its backdrop,
    /// nor over the card the cycle ends with. Once that is dismissed, it may.
    func testTheConnectScreenIsNotIdleDuringAReconnectCycle() async throws {
        let previous = UserDefaults.standard.object(forKey: "autoReconnect")
        UserDefaults.standard.set(true, forKey: "autoReconnect")
        addTeardownBlock { UserDefaults.standard.set(previous, forKey: "autoReconnect") }
        let host = FakeUDPHost()
        let manager = try await connectedManager(host, timing: ConnectionManager.Timing(
            connectTimeoutSeconds: 0.5, reconnectGraceSeconds: 0.1, reconnectBackoffSeconds: { _ in 0.4 }))
        defer { manager.cancel() }
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming") { manager.state == .connected }
        XCTAssertFalse(manager.isIdleOnConnect)

        // The PC goes away; nothing answers the reconnect attempts.
        host.sendBye(.hostShuttingDown)
        var betweenAttempts = 0
        let deadline = Date().addingTimeInterval(20)
        try await until("reconnecting") { manager.reconnectAttempt > 0 }
        while manager.reconnectAttempt > 0, Date() < deadline {
            if manager.state == .disconnected { betweenAttempts += 1 }
            XCTAssertFalse(manager.isIdleOnConnect, "idle during attempt \(manager.reconnectAttempt)")
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertGreaterThan(betweenAttempts, 0, "the cycle never waited between attempts")
        XCTAssertEqual(manager.reconnectAttempt, 0, "the cycle never ended")
        // It ends on a problem card: still not idle.
        XCTAssertNotNil(manager.problem)
        XCTAssertFalse(manager.isIdleOnConnect)
        manager.problem = nil
        XCTAssertTrue(manager.isIdleOnConnect)
    }

    /// A PC too old for this app turns it down. Its error says to update the
    /// PC, so a pending update popup (which points to TestFlight) waits until
    /// the error is dismissed.
    func testAVersionMismatchErrorKeepsTheUpdatePopupAway() async throws {
        let suite = "GoodbyeTests-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let manifest = Data(#"{"schema":1,"ipad":{"version":"9.9.9","build":999}}"#.utf8)
        let updates = UpdateChecker(defaults: defaults, source: .injected(manifest),
                                    installed: InstalledApp(version: "0.3.1", build: 166))
        updates.checkIfDue()
        XCTAssertNotNil(updates.popup, "an update is pending")

        let host = FakeUDPHost()
        let manager = ConnectionManager()
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let answered = await Task.detached { host.acceptSession(status: .versionUnsupported, acceptedVersion: 1) }.value
        XCTAssertTrue(answered, "the iPad never sent HELLO2")
        try await until("turned down") { manager.state == .disconnected && manager.connectionError != nil }
        XCTAssertEqual(manager.versionMismatch, .pcOlder)
        XCTAssertTrue(manager.connectionError?.contains("click Update now on the PC") == true, manager.connectionError ?? "")
        XCTAssertNil(manager.problem)
        XCTAssertFalse(manager.isIdleOnConnect && updates.popup != nil, "the popup must not cover the error")

        // The user dismisses the error (ConnectView's close button).
        manager.connectionError = nil
        XCTAssertTrue(manager.isIdleOnConnect && updates.popup != nil, "then the popup shows")
    }

    /// Disconnected from the PC's side: the iPad stays away.
    func testAPCDisconnectingTheIPadStaysDisconnected() async throws {
        let host = FakeUDPHost()
        let manager = try await streaming(host)
        defer { manager.cancel() }
        host.sendBye(.userDisconnect)
        try await until("disconnected") { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .pcEndedSession)
        XCTAssertEqual(manager.problem?.actions, [.retry])
        let attempts = await Task.detached { host.helloAttempts(within: 1.5) }.value
        XCTAssertEqual(attempts, 0, "no automatic reconnecting after a disconnect from the PC")
        XCTAssertEqual(manager.reconnectAttempt, 0)
    }
}
