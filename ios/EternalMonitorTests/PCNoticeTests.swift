import Combine
import XCTest
@testable import EternalMonitor

/// What a session's PC tells this iPad about itself beyond the stream.
@MainActor
final class PCNoticeTests: XCTestCase {
    private func until(_ description: String, seconds: Double = 10, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertTrue(condition(), description)
    }

    /// One session with `host`, whose HELLO_ACK carries `caps`.
    private func session(_ manager: ConnectionManager, _ host: FakeUDPHost, caps: UInt16) async throws {
        manager.connect(host: "127.0.0.1", port: host.port)
        let accepted = await Task.detached { host.acceptSession(hostCaps: caps) }.value
        XCTAssertTrue(accepted, "the iPad never sent HELLO2")
        try await until("accepted") { manager.hostInfo?.hostCaps == caps }
        manager.cancel()
    }

    /// A PC that couldn't reach Apple's USB service (Apple Devices not
    /// running or not installed) leaves the bit out of its HELLO_ACK. It is
    /// kept for that PC after the session, and cleared by the same PC's
    /// next session once the service is back.
    func testTheSightingClearsWhenThePCsServiceRecovers() async throws {
        let manager = ConnectionManager()
        addTeardownBlock { await MainActor.run { manager.cancel() } }
        XCTAssertNil(manager.appleDevicesUnreachable, "nothing known before a session")
        let host = FakeUDPHost()
        let before = Date()
        try await session(manager, host, caps: 0)
        let sighting = try XCTUnwrap(manager.appleDevicesUnreachable)
        XCTAssertEqual(sighting.target, "127.0.0.1:\(host.port)")
        XCTAssertTrue(sighting.applies(now: Date()))
        XCTAssertGreaterThanOrEqual(sighting.at, before)

        try await session(manager, host, caps: HelloAck.hostCapUSB)
        XCTAssertNil(manager.appleDevicesUnreachable)
    }

    /// Connecting to another PC drops what the last one said, at once and
    /// whether or not the new one answers.
    func testSwitchingPCsDropsTheOtherPCsSighting() async throws {
        let manager = ConnectionManager()
        addTeardownBlock { await MainActor.run { manager.cancel() } }
        let first = FakeUDPHost()
        try await session(manager, first, caps: 0)
        XCTAssertNotNil(manager.appleDevicesUnreachable)

        let second = FakeUDPHost()
        manager.connect(host: "127.0.0.1", port: second.port)
        XCTAssertNil(manager.appleDevicesUnreachable, "another PC's sighting")
        manager.cancel()
        XCTAssertNil(manager.appleDevicesUnreachable)

        // The same PC again keeps it until that PC answers.
        try await session(manager, first, caps: 0)
        manager.connect(host: "127.0.0.1", port: first.port)
        XCTAssertNotNil(manager.appleDevicesUnreachable)
        manager.cancel()
    }

    /// A sighting is one sample: it applies from when it was seen until it
    /// expires, and not under a clock set back.
    func testASightingGoesStale() {
        let at = Date(timeIntervalSince1970: 1_000_000)
        let until = at + ConnectionManager.AppleDevicesSighting.lifetime
        let sighting = ConnectionManager.AppleDevicesSighting(target: "192.168.1.20:9876", at: at, until: until)
        XCTAssertTrue(sighting.applies(now: at))
        XCTAssertTrue(sighting.applies(now: until - 1))
        XCTAssertFalse(sighting.applies(now: until))
        XCTAssertFalse(sighting.applies(now: at - 60), "a clock set back")
    }

    /// The Connect screen may sit idle after the session: the published
    /// hint goes by itself when it expires, with nothing else happening.
    func testThePublishedHintExpiresByItself() async throws {
        let seen = Date(timeIntervalSince1970: 2_000_000)
        var timing = ConnectionManager.Timing()
        timing.appleDevicesNoteSeconds = 0.5
        let manager = ConnectionManager(timing: timing)
        manager.now = { seen }
        addTeardownBlock { await MainActor.run { manager.cancel() } }
        var published: [ConnectionManager.AppleDevicesSighting?] = []
        let watch = manager.$appleDevicesUnreachable.dropFirst().sink { published.append($0) }
        defer { watch.cancel() }

        try await session(manager, FakeUDPHost(), caps: 0)
        let sighting = try XCTUnwrap(manager.appleDevicesUnreachable)
        XCTAssertEqual(sighting.at, seen)
        XCTAssertEqual(sighting.until, seen + 0.5)
        try await until("expired", seconds: 10) { manager.appleDevicesUnreachable == nil }
        XCTAssertEqual(published.last, .some(nil), "the change was published: \(published)")
        XCTAssertEqual(USBStatusText.describe("USB: waiting for a cable",
                                              appleDevicesUnreachable: manager.appleDevicesUnreachable != nil),
                       USBStatusText.describe("USB: waiting for a cable"))
    }

    /// Back in the foreground the sighting is checked against the clock,
    /// since a suspended app's timer may have missed its time.
    func testComingBackChecksTheClock() async throws {
        var clock = Date(timeIntervalSince1970: 3_000_000)
        let manager = ConnectionManager()
        manager.now = { clock }
        addTeardownBlock { await MainActor.run { manager.cancel() } }
        try await session(manager, FakeUDPHost(), caps: 0)
        let sighting = try XCTUnwrap(manager.appleDevicesUnreachable)
        XCTAssertEqual(sighting.until, clock + ConnectionManager.AppleDevicesSighting.lifetime)

        clock = sighting.until - 60
        manager.recheckAppleDevicesSighting()
        XCTAssertEqual(manager.appleDevicesUnreachable, sighting, "a minute left")
        clock = sighting.until
        manager.recheckAppleDevicesSighting()
        XCTAssertNil(manager.appleDevicesUnreachable)
    }
}
