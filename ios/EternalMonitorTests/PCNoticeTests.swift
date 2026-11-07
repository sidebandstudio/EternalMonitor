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

    /// A PC that can't reach Apple's USB service (Apple Devices not running
    /// or not installed) leaves the bit out of its HELLO_ACK. The USB card
    /// then says so; a PC that reaches it clears that.
    func testThePCsAppleDevicesStateReachesTheUSBCard() async throws {
        let manager = ConnectionManager()
        addTeardownBlock { await MainActor.run { manager.cancel() } }
        XCTAssertFalse(manager.pcLacksAppleDevices, "nothing known before a session")
        for (caps, lacks) in [(UInt16(0), true), (HelloAck.hostCapUSB, false)] {
            let host = FakeUDPHost()
            manager.connect(host: "127.0.0.1", port: host.port)
            let accepted = await Task.detached { host.acceptSession(hostCaps: caps) }.value
            XCTAssertTrue(accepted, "the iPad never sent HELLO2")
            try await until("accepted") { manager.hostInfo?.hostCaps == caps && manager.pcLacksAppleDevices == lacks }
            manager.cancel()
            XCTAssertEqual(manager.pcLacksAppleDevices, lacks, "kept after the session for the Connect screen")
        }
    }
}
