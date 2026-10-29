import XCTest
@testable import EternalMonitor

/// The one summary line of each connection attempt, as the backend accepts
/// it (b-session-summary contract), and the end each path gives it.
@MainActor
final class SessionSummaryTests: XCTestCase {
    /// The backend's check, ported from `copy_session_summaries`: the whole
    /// message matches and the numbers are in range. With
    /// ETERNAL_SUMMARY_DUMP set, the line is appended to that file too.
    static func assertContract(_ line: String, file: StaticString = #filePath, line number: UInt = #line) {
        let pattern = #"^session_summary v=1 role=(pc|ipad) session_id=(0|[1-9][0-9]{0,9}|-) nonce=([0-9a-f]{16}|-)"#
            + #" transport=(usb|wifi) mode=(mirror|second_screen|unknown)"#
            + #" stage=(started|hello_sent|accepted|video_sent|first_frame|steady)"#
            + #" outcome=(streamed|no_reply|no_picture|pairing|user_ended|app_ended|error)"#
            + #" failure=(none|no_reply|no_video|no_frame|vdd_enable_failed|vdd_attach_timeout|vdd_not_attached|capture_failed|encoder_failed|usb_lost|signal_lost|other)"#
            + #" fallback=(none|attach_timeout|enable_failed|no_driver|no_frames|duplication_failed|remembered|other)"#
            + #" end=(goodbye|liveness|superseded|shutdown|timeout|user|background|error)"#
            + #" ttff_ms=(0|[1-9][0-9]{0,5}|-) duration_ms=(0|[1-9][0-9]{0,9})"#
            + #" os=(win:[0-9]{1,6}\.[0-9]{1,6}|ipados:[0-9]{1,3}\.[0-9]{1,3}(\.[0-9]{1,3})?) hw=(pc|iPad[0-9]{1,2},[0-9]{1,2})"#
            + #" gpu=(-|AMD(\+Intel)?(\+NVIDIA)?(\+other)?|Intel(\+NVIDIA)?(\+other)?|NVIDIA(\+other)?|other) hybrid=(true|false|-)$"#
        XCTAssertLessThanOrEqual(line.utf8.count, 400, line, file: file, line: number)
        XCTAssertNotNil(line.range(of: pattern, options: .regularExpression), line, file: file, line: number)
        func value(_ key: String) -> Int {
            line.split(separator: " ").first { $0.hasPrefix(key) }.flatMap { Int($0.dropFirst(key.count)) } ?? 0
        }
        XCTAssertLessThanOrEqual(value("session_id="), 4_294_967_295, line, file: file, line: number)
        XCTAssertLessThanOrEqual(value("ttff_ms="), 600_000, line, file: file, line: number)
        XCTAssertLessThanOrEqual(value("duration_ms="), 2_592_000_000, line, file: file, line: number)
        if let path = ProcessInfo.processInfo.environment["ETERNAL_SUMMARY_DUMP"],
           let handle = FileHandle(forWritingAtPath: path) ?? {
               FileManager.default.createFile(atPath: path, contents: nil)
               return FileHandle(forWritingAtPath: path)
           }() {
            handle.seekToEndOfFile()
            handle.write(Data((line + "\n").utf8))
            handle.closeFile()
        }
    }

    /// stage, outcome, failure and end, as `line` gives them.
    private func tokens(_ line: String) -> [String] {
        ["stage=", "outcome=", "failure=", "end="].map { key in
            line.split(separator: " ").first { $0.hasPrefix(key) }.map { String($0.dropFirst(key.count)) } ?? ""
        }
    }

    private func summary(_ end: AttemptEnd, usb: Bool = false, session: UInt32? = 41, nonce: UInt32 = 0xE2E0_0001,
                         firstFrame: TimeInterval? = nil, duration: TimeInterval = 2) -> SessionSummary {
        SessionSummary(end: end, isUSB: usb, sessionId: session, nonce: nonce, firstFrame: firstFrame,
                       duration: duration, os: "ipados:18.5", hardware: "iPad16,3")
    }

    func testAStreamedAttemptIsTheContractLine() {
        let line = summary(.userDisconnect, firstFrame: 0.412, duration: 61.234).line
        XCTAssertEqual(line, "session_summary v=1 role=ipad session_id=41 nonce=00000000e2e00001 transport=wifi"
            + " mode=unknown stage=steady outcome=streamed failure=none fallback=none end=user ttff_ms=412"
            + " duration_ms=61234 os=ipados:18.5 hw=iPad16,3 gpu=- hybrid=-")
        Self.assertContract(line)
    }

    func testEachWayAnAttemptEndsHasItsSummary() {
        var noFrame = summary(.connectTimeout, duration: 10)
        noFrame.datagramsReceived = 12
        var pairing = summary(.userDisconnect, session: nil, duration: 30)
        pairing.awaitingPairing = true
        var retried = summary(.retriedWithCode, session: nil, duration: 4)
        retried.awaitingPairing = true
        let cases: [(SessionSummary, [String])] = [
            (summary(.userDisconnect), ["accepted", "user_ended", "none", "user"]),
            (summary(.usbTurnedOff, usb: true, firstFrame: 1, duration: 5), ["first_frame", "streamed", "none", "user"]),
            (summary(.appBackground, firstFrame: 1, duration: 20), ["steady", "streamed", "none", "background"]),
            (summary(.appBackground), ["accepted", "app_ended", "none", "background"]),
            (summary(.superseded), ["accepted", "app_ended", "none", "superseded"]),
            (summary(.pcGoodbye), ["accepted", "app_ended", "none", "goodbye"]),
            (summary(.pcRefused, session: nil), ["hello_sent", "error", "other", "error"]),
            (pairing, ["hello_sent", "pairing", "none", "user"]),
            (summary(.legacyPC, session: nil), ["hello_sent", "error", "other", "error"]),
            (summary(.connectTimeout, session: nil, duration: 10), ["hello_sent", "no_reply", "no_reply", "timeout"]),
            (summary(.connectTimeout, duration: 10), ["accepted", "no_picture", "no_video", "timeout"]),
            (noFrame, ["accepted", "no_picture", "no_frame", "timeout"]),
            (summary(.pcStoppedResponding, firstFrame: 0.3, duration: 90), ["steady", "streamed", "signal_lost", "liveness"]),
            (summary(.pictureStalled, firstFrame: 0.3, duration: 90), ["steady", "streamed", "other", "error"]),
            (summary(.cableUnplugged, usb: true, firstFrame: 0.3), ["first_frame", "streamed", "usb_lost", "error"]),
            (summary(.usbLinkFailed, usb: true), ["accepted", "error", "usb_lost", "error"]),
            (summary(.closedBeforePicture, usb: true), ["accepted", "no_picture", "usb_lost", "error"]),
            (summary(.linkFailed, session: nil, nonce: 0, duration: 0.2), ["started", "error", "other", "error"]),
            (summary(.receiverFailed, session: nil, nonce: 0, duration: 0), ["started", "error", "other", "error"]),
            (retried, ["hello_sent", "pairing", "none", "superseded"]),
        ]
        for (summary, expected) in cases {
            XCTAssertEqual(tokens(summary.line), expected, "\(summary.end)")
            Self.assertContract(summary.line)
        }
        let started = summary(.linkFailed, session: nil, nonce: 0).line
        XCTAssertTrue(started.contains(" session_id=- nonce=- "), started)
        XCTAssertTrue(summary(.cableUnplugged, usb: true).line.contains(" transport=usb "))
    }

    func testOnlyTheUsersOwnActionSaysUserDisconnectToThePC() {
        XCTAssertEqual(AttemptEnd.userDisconnect.bye, .userDisconnect)
        XCTAssertEqual(AttemptEnd.usbTurnedOff.bye, .userDisconnect)
        XCTAssertEqual(AttemptEnd.appBackground.bye, .appBackground)
        XCTAssertEqual(AttemptEnd.superseded.bye, .superseded)
        for end in [AttemptEnd.pcGoodbye, .pcRefused, .legacyPC, .connectTimeout, .pcStoppedResponding,
                    .pictureStalled, .cableUnplugged, .usbLinkFailed, .closedBeforePicture, .linkFailed,
                    .receiverFailed, .retriedWithCode] {
            XCTAssertEqual(end.bye, .error, "\(end)")
        }
        XCTAssertEqual(AttemptEnd.linkFailed.name, "linkFailed")
    }

    func testNumbersOSAndModelStayInTheContract() {
        let long = summary(.pcStoppedResponding, session: UInt32.max, nonce: UInt32.max, firstFrame: 4000,
                           duration: 40 * 86_400).line
        XCTAssertTrue(long.contains(" session_id=4294967295 nonce=00000000ffffffff "), long)
        XCTAssertTrue(long.contains(" ttff_ms=600000 duration_ms=2592000000 "), long)
        Self.assertContract(long)
        XCTAssertEqual(SessionSummary.osToken(OperatingSystemVersion(majorVersion: 18, minorVersion: 5, patchVersion: 0)), "ipados:18.5")
        XCTAssertEqual(SessionSummary.osToken(OperatingSystemVersion(majorVersion: 26, minorVersion: 0, patchVersion: 1)), "ipados:26.0.1")
        XCTAssertEqual(SessionSummary.osToken(OperatingSystemVersion(majorVersion: 1000, minorVersion: 0, patchVersion: 0)), "ipados:0.0")
        XCTAssertEqual(SessionSummary.hardwareToken("iPad13,8"), "iPad13,8")
        for machine in ["arm64", "iPhone17,1", "iPad13,8 ", "iPad123,1", "Ali's iPad"] {
            XCTAssertEqual(SessionSummary.hardwareToken(machine), "iPad0,0", machine)
        }
        var here = summary(.userDisconnect)
        here.os = SessionSummary.currentOS
        here.hardware = SessionSummary.currentHardware
        Self.assertContract(here.line)
    }

    // MARK: - The real paths

    private func until(_ description: String, seconds: Double = 10, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    private func logged(_ manager: ConnectionManager, _ category: String) -> [String] {
        manager.diagnostics.filter { $0.category == category }.map(\.message)
    }

    /// Network.framework reports Local Network denied before the socket is
    /// ready: the app ended the attempt, the user didn't.
    func testLocalNetworkDeniedIsALinkFailureNotTheUsersDisconnect() async throws {
        let link = DeniedLink()
        let manager = ConnectionManager(udpReceiver: { _ in link })
        manager.connect(host: "192.168.1.20", port: 9876)
        XCTAssertEqual(manager.state, .connecting)
        link.onError?(LinkError(.localNetworkDenied, "UDP path not ready: Local network prohibited"))
        try await until("ended") { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .localNetworkBlocked)
        XCTAssertEqual(logged(manager, "connection").filter { $0.hasPrefix("Disconnect ") },
                       ["Disconnect state=connecting reason=linkFailed"])
        let summaries = logged(manager, "summary")
        XCTAssertEqual(summaries.count, 1, "\(summaries)")
        let line = summaries.first ?? ""
        XCTAssertTrue(line.hasPrefix("session_summary v=1 role=ipad session_id=- nonce=- transport=wifi mode=unknown"
            + " stage=started outcome=error failure=other fallback=none end=error ttff_ms=- duration_ms="), line)
        XCTAssertFalse(line.contains("192.168"), line)
        Self.assertContract(line)
    }

    /// Try again opens a new connection, which asks iPadOS again: once
    /// Local Network is on, the old denial isn't shown or remembered.
    func testTryAgainAfterLocalNetworkDeniedAsksIPadOSAgain() async throws {
        let links = Links()
        let manager = ConnectionManager(udpReceiver: { _ in links.make() })
        manager.connect(host: "192.168.1.20", port: 9876)
        links.all.last?.onError?(LinkError(.localNetworkDenied, "UDP path not ready: Local network prohibited"))
        try await until("blocked") { manager.problem == .localNetworkBlocked }
        XCTAssertEqual(manager.problem?.actions, [.openSettings, .retry])

        manager.retry()
        XCTAssertEqual(links.all.count, 2, "a new connection, not the failed one")
        XCTAssertEqual(manager.state, .connecting)
        XCTAssertNil(manager.problem)
        // Still off: the new connection says so again.
        links.all.last?.onError?(LinkError(.localNetworkDenied, "UDP path not ready: Local network prohibited"))
        try await until("blocked again") { manager.problem == .localNetworkBlocked }

        // Turned on: the third connection opens, and nothing says Local Network.
        manager.retry()
        XCTAssertEqual(links.all.count, 3)
        links.all.last?.onListenerReady?(50000)
        links.all.last?.onConnectionEstablished?()
        try await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertEqual(manager.state, .connecting)
        XCTAssertNil(manager.problem)
        manager.disconnect()
    }

    /// Heartbeats stop during a session: after the grace the app ends it as
    /// `pcStoppedResponding`, tells the PC `error`, and the summary says
    /// liveness with the session and the nonce the PC answered.
    func testAPCThatStopsRespondingEndsTheSessionByLiveness() async throws {
        let previous = UserDefaults.standard.object(forKey: "autoReconnect")
        UserDefaults.standard.set(false, forKey: "autoReconnect")
        defer { UserDefaults.standard.set(previous, forKey: "autoReconnect") }
        let host = FakeUDPHost()
        let manager = ConnectionManager()
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let nonce = await Task.detached { host.acceptSession() ? host.lastNonce : nil }.value
        let answered = try XCTUnwrap(nonce, "the iPad never sent HELLO2")
        try await until("handshake") { manager.hostInfo != nil }
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming") { manager.state == .connected }
        // No heartbeats: 3 s liveness, then 5 s with the signal lost.
        try await until("gave up", seconds: 15) { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .lost(.pcStoppedResponding))
        XCTAssertTrue(logged(manager, "connection").contains("Disconnect state=connected reason=pcStoppedResponding"))
        let reasons = await Task.detached { host.goodbyes() }.value
        XCTAssertEqual(reasons, Array(repeating: .error, count: ControlChannel.byeCopies))
        let line = logged(manager, "summary").first ?? ""
        XCTAssertTrue(line.hasPrefix("session_summary v=1 role=ipad session_id=41 nonce=\(SessionSummary.hex16(answered))"
            + " transport=wifi mode=unknown stage=first_frame outcome=streamed failure=signal_lost fallback=none"
            + " end=liveness ttff_ms="), line)
        Self.assertContract(line)
    }

    /// A refused HELLO2, a wrong code, then the right one: three nonces, and
    /// a line for each, as the PC writes.
    func testEachPairingAttemptHasItsOwnSummary() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager()
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        var nonces: [UInt32] = []
        for (round, status) in [HelloStatus.unauthorized, .unauthorized, .ok].enumerated() {
            let nonce = await Task.detached { host.acceptSession(status: status) ? host.lastNonce : nil }.value
            nonces.append(try XCTUnwrap(nonce, "HELLO2 \(round + 1) never came"))
            guard status != .ok else { break }
            try await until("prompt \(round + 1)") {
                manager.diagnostics.filter { $0.message == "Pairing code required" }.count == round + 1
            }
            let prompt = try XCTUnwrap(manager.pairingPrompt)
            prompt.code = "123456"
            manager.submitPairing()
        }
        try await until("paired") { manager.hostInfo != nil }
        manager.cancel()
        let summaries = logged(manager, "summary")
        XCTAssertEqual(summaries.count, 3, "\(summaries)")
        guard summaries.count == 3 else { return }
        XCTAssertEqual(Set(nonces).count, 3)
        for (index, line) in summaries.enumerated() {
            XCTAssertTrue(line.contains(" nonce=\(SessionSummary.hex16(nonces[index])) "), line)
            Self.assertContract(line)
        }
        for line in summaries.prefix(2) {
            XCTAssertTrue(line.contains(" session_id=- "), line)
            XCTAssertEqual(tokens(line), ["hello_sent", "pairing", "none", "superseded"])
        }
        XCTAssertTrue(summaries[2].contains(" session_id=41 "), summaries[2])
        XCTAssertEqual(tokens(summaries[2]), ["accepted", "user_ended", "none", "user"])
    }

    /// Cancel right after a code goes out, while the control queue hasn't
    /// sent the new HELLO2 yet: the refused nonce and the new one get one
    /// summary each.
    func testCancellingWhileACodeGoesOutSummarizesEachNonceOnce() async throws {
        let host = FakeUDPHost()
        var receiver: UDPReceiver?
        let manager = ConnectionManager(udpReceiver: { port in
            let link = UDPReceiver(port: port)
            receiver = link
            return link
        })
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let nonce = await Task.detached { host.acceptSession(status: .unauthorized) ? host.lastNonce : nil }.value
        let refused = try XCTUnwrap(nonce, "the iPad never sent HELLO2")
        try await until("prompt") { manager.pairingPrompt != nil }
        let queue = try XCTUnwrap(receiver).controlQueue
        let held = DispatchSemaphore(value: 0)
        queue.async { held.wait() }
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.2) { held.signal() }
        manager.pairingPrompt?.code = "123456"
        manager.submitPairing()
        manager.cancel()
        let summaries = logged(manager, "summary")
        XCTAssertEqual(summaries.count, 2, "\(summaries)")
        XCTAssertTrue(summaries.first?.contains(" nonce=\(SessionSummary.hex16(refused)) ") == true, "\(summaries)")
        let nonces = summaries.compactMap { $0.split(separator: " ").first { $0.hasPrefix("nonce=") } }
        XCTAssertEqual(Set(nonces).count, 2, "\(summaries)")
        XCTAssertEqual(tokens(summaries.last ?? ""), ["hello_sent", "pairing", "none", "user"])
        summaries.forEach { Self.assertContract($0) }
    }

    /// The user's Disconnect is the one `userDisconnect`, with one summary.
    func testTheUsersDisconnectEndsWithOneSummary() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager()
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted)
        try await until("handshake") { manager.hostInfo != nil }
        manager.cancel()
        manager.cancel()
        XCTAssertTrue(logged(manager, "connection").contains("Disconnect state=connecting reason=userDisconnect"))
        let summaries = logged(manager, "summary")
        XCTAssertEqual(summaries.count, 1, "\(summaries)")
        XCTAssertEqual(tokens(summaries.first ?? ""), ["accepted", "user_ended", "none", "user"])
        Self.assertContract(summaries.first ?? "")
    }
}

/// A Wi-Fi link that never gets ready, as when Local Network access is off.
private final class Links {
    private(set) var all: [DeniedLink] = []
    func make() -> DeniedLink {
        let link = DeniedLink()
        all.append(link)
        return link
    }
}

private final class DeniedLink: MediaLink {
    let datagrams = MediaDatagrams()
    let controlQueue = DispatchQueue(label: "test.denied-link")
    var onConnectionEstablished: (() -> Void)?
    var onError: ((LinkError) -> Void)?
    var onListenerReady: ((UInt16) -> Void)?
    var onClosed: (() -> Void)?
    func start(host: String) -> Bool { true }
    func send(_ data: Data) {}
    func stop() {}
}
