import Darwin
import Network
import XCTest
@testable import EternalMonitor

/// A real TCP-backed FramedLink, as the USB tunnel delivers it, failing or
/// closing while a connection is being set up. FramedLink reports a failure
/// with onError and then onClosed; only an ordinary close is a removed cable.
@MainActor
final class USBLinkFailureTests: XCTestCase {
    private var previousAuto: Any?
    private var previousHost: Any?
    private var previousPort: Any?
    /// Every socket a test opened, closed at teardown.
    private var sockets: [Int32] = []
    /// The far end of the newest cable.
    private var peer: Int32 = -1

    override func setUp() async throws {
        previousAuto = UserDefaults.standard.object(forKey: "autoReconnect")
        previousHost = UserDefaults.standard.object(forKey: "lastHost")
        previousPort = UserDefaults.standard.object(forKey: "lastPort")
        UserDefaults.standard.set(false, forKey: "autoReconnect")
    }

    override func tearDown() async throws {
        UserDefaults.standard.set(previousAuto, forKey: "autoReconnect")
        UserDefaults.standard.set(previousHost, forKey: "lastHost")
        UserDefaults.standard.set(previousPort, forKey: "lastPort")
        sockets.forEach { Darwin.close($0) }
    }

    /// A ConnectionManager connecting over a FramedLink whose far end is
    /// `peer`, past the preamble.
    private func connectOverCable(timing: ConnectionManager.Timing = .init()) async throws -> (ConnectionManager, FramedLink) {
        let link = try await openCable()
        let manager = ConnectionManager(timing: timing)
        manager.connect(target: .usb(link))
        XCTAssertEqual(manager.state, .connecting)
        return (manager, link)
    }

    /// A FramedLink past the preamble, as the USB listener accepts it, whose
    /// far end becomes `peer`.
    private func openCable() async throws -> FramedLink {
        let server = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP)
        sockets.append(server)
        XCTAssertGreaterThanOrEqual(server, 0)
        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        var size = socklen_t(MemoryLayout<sockaddr_in>.size)
        let bindSize = size
        let port = withUnsafeMutablePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                XCTAssertEqual(Darwin.bind(server, $0, bindSize), 0)
                XCTAssertEqual(getsockname(server, $0, &size), 0)
            }
            return UInt16(bigEndian: pointer.pointee.sin_port)
        }
        XCTAssertEqual(listen(server, 1), 0)
        var timeout = timeval(tv_sec: 3, tv_usec: 0)
        XCTAssertEqual(setsockopt(server, SOL_SOCKET, SO_RCVTIMEO, &timeout,
            socklen_t(MemoryLayout<timeval>.size)), 0)
        let connection = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        let link = FramedLink(connection: connection)
        let ready = expectation(description: "Valid USB preamble")
        link.onPreambleReady = { ready.fulfill() }
        link.validatePreamble()
        peer = accept(server, nil, nil)
        XCTAssertGreaterThanOrEqual(peer, 0)
        sockets.append(peer)
        _ = EMLinkFramer.preamble.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        await fulfillment(of: [ready], timeout: 3)
        return link
    }

    private func waitUntilDisconnected(_ manager: ConnectionManager) async throws {
        for _ in 0..<200 where manager.state != .disconnected {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        // Let any callback still queued behind the failure run.
        try await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertEqual(manager.state, .disconnected)
    }

    func testCableFailureDuringSetupKeepsCableAdviceAfterTheLinkCloses() async throws {
        let (manager, link) = try await connectOverCable()
        defer { link.stop(); manager.cancel() }
        // A framing failure: FramedLink.fail sends onError, then onClosed.
        let invalid = Data([0, 0])
        _ = invalid.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        try await waitUntilDisconnected(manager)
        XCTAssertEqual(manager.problem, .usbFailed,
            "A failed USB link must keep the cable repair advice and Try again")
        XCTAssertEqual(manager.problem?.actions, [.retry])
        XCTAssertEqual(manager.usbStatus, "USB: waiting for a cable")
    }

    func testOrdinaryCloseDuringSetupSaysTheCableWasDisconnected() async throws {
        let (manager, link) = try await connectOverCable()
        defer { link.stop(); manager.cancel() }
        // The far end closes cleanly: onClosed alone, as when the cable comes out.
        // A FIN, not close(): closing with the iPad's unread HELLO queued
        // would send a reset, which the link rightly reports as a failure.
        XCTAssertEqual(shutdown(peer, SHUT_WR), 0)
        try await waitUntilDisconnected(manager)
        XCTAssertEqual(manager.problem, .lost(.cableUnplugged))
        XCTAssertEqual(manager.usbStatus, "USB: waiting for a cable")
    }

    // MARK: Automatic reconnecting on: a cable that fails or closes during
    // setup falls back to the saved Wi-Fi PC, once.

    /// Saves `host` as the last Wi-Fi PC and turns automatic reconnecting on.
    private func enableWiFiFallback(to host: FakeUDPHost) {
        UserDefaults.standard.set(true, forKey: "autoReconnect")
        UserDefaults.standard.set("127.0.0.1", forKey: "lastHost")
        UserDefaults.standard.set(Int(host.port), forKey: "lastPort")
    }

    private func assertFallsBackOnce(_ manager: ConnectionManager, to wifi: FakeUDPHost) async throws {
        try await waitUntilDisconnected(manager)
        // Falling back, not giving up: the banner says what is happening.
        XCTAssertEqual(manager.problem, .reconnecting(attempt: 1, of: 6))
        XCTAssertEqual(manager.diagnostics.filter { $0.message.hasPrefix("Connection lost") }.count, 1,
            "the reconnect decision must run exactly once")
        // The first retry waits one second; allow four.
        let attempts = await Task.detached { wifi.helloAttempts(within: 4) }.value
        XCTAssertEqual(attempts, 1, "exactly one Wi-Fi fallback attempt")
    }

    func testCableFailureDuringSetupFallsBackToWiFiOnce() async throws {
        let wifi = FakeUDPHost()
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable()
        defer { link.stop(); manager.cancel() }
        let invalid = Data([0, 0])
        _ = invalid.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        try await assertFallsBackOnce(manager, to: wifi)
    }

    func testCleanCloseDuringSetupFallsBackToWiFiOnce() async throws {
        let wifi = FakeUDPHost()
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable()
        defer { link.stop(); manager.cancel() }
        XCTAssertEqual(shutdown(peer, SHUT_WR), 0)
        try await assertFallsBackOnce(manager, to: wifi)
    }

    /// Reconnect timing short enough to run a whole cycle in seconds.
    private let fast = ConnectionManager.Timing(connectTimeoutSeconds: 0.4, reconnectGraceSeconds: 0.1,
        reconnectBackoffSeconds: { _ in 0.05 })

    /// Waits for the reconnect cycle to start and then to run out.
    private func waitForFallbackToGiveUp(_ manager: ConnectionManager) async throws {
        var started = false
        for _ in 0..<1000 {
            if manager.reconnectAttempt > 0 { started = true }
            if started && manager.reconnectAttempt == 0 { return }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("the Wi-Fi fallback never finished")
    }

    /// Every Wi-Fi fallback attempt times out: the user ends on the cable
    /// advice and Try again, not "Couldn't reconnect". Try again then goes
    /// back to the cable the advice is about, not to Wi-Fi.
    func testCableFailureAdviceSurvivesAnExhaustedWiFiFallback() async throws {
        let previousUSB = UserDefaults.standard.object(forKey: "allowUSB")
        defer { UserDefaults.standard.set(previousUSB, forKey: "allowUSB") }
        // Try again must not open the app's real USB listener port.
        UserDefaults.standard.set(false, forKey: "allowUSB")
        let wifi = FakeUDPHost()  // never answers
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable(timing: fast)
        defer { link.stop(); manager.cancel() }
        let attempts = Task.detached { wifi.helloAttempts(within: 6) }
        let invalid = Data([0, 0])
        _ = invalid.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        try await waitForFallbackToGiveUp(manager)
        let attempted = await attempts.value
        XCTAssertEqual(attempted, 6, "all six Wi-Fi attempts ran")
        XCTAssertEqual(manager.state, .disconnected)
        XCTAssertEqual(manager.reconnectAttempt, 0)
        XCTAssertEqual(manager.problem, .usbFailed)
        XCTAssertEqual(manager.problem?.actions, [.retry])
        manager.retry()
        XCTAssertEqual(manager.state, .disconnected, "Try again waits for the cable instead of connecting over Wi-Fi")
        XCTAssertNil(manager.problem)
        XCTAssertEqual(manager.usbStatus, "USB: off", "Try again went back to the cable listener")
        let retried = await Task.detached { wifi.helloAttempts(within: 1) }.value
        XCTAssertEqual(retried, 0, "no Wi-Fi attempt under the cable advice")
    }

    /// A later, more specific failure (the saved address doesn't resolve)
    /// replaces the cable advice when the fallback gives up.
    func testALaterSpecificFailureReplacesTheCableAdvice() async throws {
        UserDefaults.standard.set(true, forKey: "autoReconnect")
        UserDefaults.standard.set("eternalmonitor-test.invalid", forKey: "lastHost")
        UserDefaults.standard.set(9876, forKey: "lastPort")
        let (manager, link) = try await connectOverCable(timing: fast)
        defer { link.stop(); manager.cancel() }
        let invalid = Data([0, 0])
        _ = invalid.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        try await waitForFallbackToGiveUp(manager)
        XCTAssertEqual(manager.reconnectAttempt, 0)
        XCTAssertEqual(manager.problem, .badAddress("eternalmonitor-test.invalid"))
    }

    // MARK: Who owns the reconnect cycle. A connection the user or a cable
    // starts replaces the automatic cycle, and a replaced cycle writes nothing.

    /// Waits between attempts long enough to tap Try again on a timed-out one.
    private let slowCycle = ConnectionManager.Timing(connectTimeoutSeconds: 0.4, reconnectGraceSeconds: 0.6,
        reconnectBackoffSeconds: { _ in 0.3 })

    private func until(_ description: String, seconds: Double = 10, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    private func injectFramingFailure() {
        let invalid = Data([0, 0])
        XCTAssertEqual(invalid.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }, 2)
    }

    /// A cable failure starts the fallback to `wifi`, which never answers.
    /// Returns once its first attempt has timed out and offers Try again.
    private func failCableIntoFallback(to wifi: FakeUDPHost, timing: ConnectionManager.Timing) async throws
        -> (ConnectionManager, FramedLink) {
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable(timing: timing)
        injectFramingFailure()
        try await until("the first Wi-Fi attempt times out") {
            manager.transportMode == "WiFi" && manager.state == .disconnected && manager.problem == .noReply(usb: false)
        }
        XCTAssertEqual(manager.reconnectAttempt, 1)
        return (manager, link)
    }

    func testTryAgainDuringTheFallbackEndsIt() async throws {
        let wifi = FakeUDPHost()
        let attempts = Task.detached { wifi.helloAttempts(within: 5) }
        let (manager, link) = try await failCableIntoFallback(to: wifi, timing: slowCycle)
        defer { link.stop(); manager.cancel() }
        XCTAssertEqual(manager.problem?.actions, [.retry])
        manager.retry()
        XCTAssertEqual(manager.state, .connecting)
        XCTAssertNil(manager.problem)
        XCTAssertEqual(manager.reconnectAttempt, 0, "Try again ends the automatic cycle")
        try await until("Try again times out") { manager.state == .disconnected }
        try await Task.sleep(for: .seconds(2))
        XCTAssertEqual(manager.problem, .noReply(usb: false),
            "the old cycle must not replace Try again's result with the cable advice")
        XCTAssertEqual(manager.reconnectAttempt, 0)
        let attempted = await attempts.value
        XCTAssertEqual(attempted, 2, "the fallback's first attempt and Try again, nothing after")
    }

    func testConnectingToAnotherPCDuringTheFallbackEndsIt() async throws {
        let wifi = FakeUDPHost()
        let other = FakeUDPHost()  // never answers either
        let attempts = Task.detached { wifi.helloAttempts(within: 5) }
        let (manager, link) = try await failCableIntoFallback(to: wifi, timing: slowCycle)
        defer { link.stop(); manager.cancel() }
        manager.connect(host: "127.0.0.1", port: other.port)
        XCTAssertEqual(manager.state, .connecting)
        XCTAssertNil(manager.problem)
        XCTAssertEqual(manager.reconnectAttempt, 0, "another PC ends the automatic cycle")
        try await until("the other PC times out") { manager.state == .disconnected }
        try await Task.sleep(for: .seconds(2))
        XCTAssertEqual(manager.problem, .noReply(usb: false))
        XCTAssertEqual(manager.reconnectAttempt, 0)
        let attempted = await attempts.value
        XCTAssertEqual(attempted, 1, "the fallback stops after its first attempt")
    }

    /// The PC the user picks answers only after the replaced cycle would have
    /// finished waiting for its own attempt. That cycle must not write its
    /// progress over the session that then streams.
    func testAReplacedCycleLeavesASuccessfulConnectionAlone() async throws {
        let wifi = FakeUDPHost()
        let other = FakeUDPHost()
        let timing = ConnectionManager.Timing(connectTimeoutSeconds: 1.5, reconnectGraceSeconds: 0.5,
            reconnectBackoffSeconds: { _ in 0.05 })
        let (manager, link) = try await failCableIntoFallback(to: wifi, timing: timing)
        defer { link.stop(); manager.cancel() }
        manager.connect(host: "127.0.0.1", port: other.port)
        // The replaced cycle would wake about 0.5 s from now.
        let accepted = await Task.detached { Thread.sleep(forTimeInterval: 1); return other.acceptSession() }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2 to the other PC")
        try await until("handshake") { manager.hostInfo != nil }
        other.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming") { manager.state == .connected }
        try await Task.sleep(for: .milliseconds(500))
        XCTAssertEqual(manager.state, .connected)
        XCTAssertNil(manager.problem, "a replaced cycle must not show its progress over a working session")
        XCTAssertEqual(manager.reconnectAttempt, 0)
    }

    /// Backgrounding during the fallback ends it where running out would;
    /// foregrounding resumes with one attempt the old cycle can't touch.
    func testBackgroundingDuringTheFallbackEndsItAndForegroundingResumes() async throws {
        let defaults = UserDefaults.standard
        let previous = ["allowUSB", "autoResumeOnForeground"].map { ($0, defaults.object(forKey: $0)) }
        defer { previous.forEach { defaults.set($0.1, forKey: $0.0) } }
        // Foregrounding must not open the app's real USB listener port.
        defaults.set(false, forKey: "allowUSB")
        defaults.set(true, forKey: "autoResumeOnForeground")
        let wifi = FakeUDPHost()
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable(timing: fast)
        defer { link.stop(); manager.cancel() }
        injectFramingFailure()
        try await until("a Wi-Fi attempt is under way") { manager.transportMode == "WiFi" && manager.state == .connecting }
        manager.handleAppBackgrounded()
        XCTAssertEqual(manager.state, .disconnected)
        XCTAssertEqual(manager.reconnectAttempt, 0, "backgrounding ends the automatic cycle")
        XCTAssertEqual(manager.problem, .usbFailed, "the user comes back to the cable advice")
        try await Task.sleep(for: .seconds(1))
        XCTAssertEqual(manager.state, .disconnected, "no reconnecting in the background")
        XCTAssertEqual(manager.problem, .usbFailed)
        manager.handleAppForegrounded()
        XCTAssertEqual(manager.state, .connecting, "foregrounding resumes")
        XCTAssertNil(manager.problem)
        try await until("the resumed attempt times out") { manager.state == .disconnected }
        try await Task.sleep(for: .seconds(1))
        XCTAssertEqual(manager.problem, .noReply(usb: false))
        XCTAssertEqual(manager.reconnectAttempt, 0)
    }

    /// A cable plugged in again ends the fallback, whether it is waiting
    /// between attempts or has a Wi-Fi attempt under way.
    func testReplugDuringTheFallbackEndsIt() async throws {
        let wifi = FakeUDPHost()
        enableWiFiFallback(to: wifi)
        for underWay in [false, true] {
            let (manager, link) = try await connectOverCable(timing: slowCycle)
            defer { link.stop(); manager.cancel() }
            injectFramingFailure()
            try await until("the fallback started") { manager.reconnectAttempt == 1 }
            if underWay {
                try await until("a Wi-Fi attempt is under way") {
                    manager.transportMode == "WiFi" && manager.state == .connecting
                }
            }
            let cable = try await openCable()
            defer { cable.stop() }
            manager.acceptUSBLink(cable)
            XCTAssertEqual(manager.transportMode, "USB", "under way: \(underWay)")
            XCTAssertEqual(manager.state, .connecting, "under way: \(underWay)")
            XCTAssertEqual(manager.reconnectAttempt, 0, "the cable ends the automatic cycle")
            try await until("the cable attempt times out") { manager.state == .disconnected }
            try await Task.sleep(for: .seconds(1.5))
            XCTAssertEqual(manager.transportMode, "USB", "no Wi-Fi attempt after the cable came back")
            XCTAssertEqual(manager.problem, .noReply(usb: true), "under way: \(underWay)")
            XCTAssertEqual(manager.reconnectAttempt, 0)
        }
    }

    // MARK: Answers from the PC during the fallback. Busy and a restart pass;
    // the cycle keeps retrying.

    /// Long enough to answer the handshake and stream within one attempt.
    private let answering = ConnectionManager.Timing(connectTimeoutSeconds: 1, reconnectGraceSeconds: 0.1,
        reconnectBackoffSeconds: { _ in 0.05 })

    private func finishConnecting(_ manager: ConnectionManager, to host: FakeUDPHost) async throws {
        let accepted = await Task.detached { host.acceptSession() }.value
        XCTAssertTrue(accepted, "the cycle never tried the PC again")
        try await until("handshake") { manager.hostInfo != nil }
        host.sendKeyframe(try FakeUDPHost.keyframe())
        try await until("streaming") { manager.state == .connected }
        XCTAssertNil(manager.problem)
        XCTAssertEqual(manager.reconnectAttempt, 0)
    }

    /// The PC still holds this iPad's cable session and answers the Wi-Fi
    /// fallback with Busy, then frees up.
    func testBusyDuringTheFallbackRetriesUntilThePCIsFree() async throws {
        let wifi = FakeUDPHost()
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable(timing: answering)
        defer { link.stop(); manager.cancel() }
        injectFramingFailure()
        let refused = await Task.detached { wifi.acceptSession(status: .busy) }.value
        XCTAssertTrue(refused)
        try await until("the refusal shows") { manager.state == .disconnected && manager.problem == .pcBusy }
        XCTAssertEqual(manager.reconnectAttempt, 1, "Busy keeps the automatic cycle")
        try await finishConnecting(manager, to: wifi)
    }

    /// A PC that stays busy: the user ends on its reason and Try again.
    func testBusyThroughoutTheFallbackEndsOnTheRefusal() async throws {
        let wifi = FakeUDPHost()
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable(timing: fast)
        defer { link.stop(); manager.cancel() }
        let refusals = Task.detached { (0..<6).filter { _ in wifi.acceptSession(status: .busy) }.count }
        injectFramingFailure()
        try await waitForFallbackToGiveUp(manager)
        let refused = await refusals.value
        XCTAssertEqual(refused, 6, "every attempt ran")
        XCTAssertEqual(manager.problem, .pcBusy)
        XCTAssertEqual(manager.problem?.actions, [.retry])
    }

    /// The PC says it is shutting down during the fallback (Stop and update
    /// on the PC) and comes back on the new version.
    func testAPCRestartingDuringTheFallbackIsRetried() async throws {
        let wifi = FakeUDPHost()
        enableWiFiFallback(to: wifi)
        let (manager, link) = try await connectOverCable(timing: answering)
        defer { link.stop(); manager.cancel() }
        injectFramingFailure()
        let accepted = await Task.detached { wifi.acceptSession() }.value
        XCTAssertTrue(accepted)
        try await until("handshake") { manager.hostInfo != nil }
        wifi.sendBye(.hostShuttingDown)
        try await until("the goodbye ends that attempt") { manager.state == .disconnected }
        XCTAssertEqual(manager.problem, .pcUpdating(.reconnecting))
        XCTAssertEqual(manager.reconnectAttempt, 1, "a restarting PC keeps the automatic cycle")
        try await finishConnecting(manager, to: wifi)
    }
}
