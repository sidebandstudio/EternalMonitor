import Network
import XCTest
@testable import EternalMonitor

final class ConnectionProblemTests: XCTestCase {
    func testTimeoutsMapToHowFarTheAttemptGot() {
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: false, helloAttempts: 0, handshakeDone: false, datagramsReceived: 0, isUSB: false), .receiverNotReady)
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 0, handshakeDone: false, datagramsReceived: 0, isUSB: false), .receiverNotReady)
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 6, handshakeDone: false, datagramsReceived: 0, isUSB: false), .noReply(usb: false))
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 6, handshakeDone: false, datagramsReceived: 0, isUSB: true), .noReply(usb: true))
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 2, handshakeDone: false, datagramsReceived: 40, isUSB: false), .noPicture)
        // The PC accepted the session but sent no video: its capture or encoder
        // failed. That is not a network problem, whatever arrived.
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 1, handshakeDone: true, datagramsReceived: 0, isUSB: true), .noPicture)
    }

    func testAPairingCodeTypedAsTheAddressIsCaughtBeforeConnecting() {
        for typed in ["390756", "390 756", " 390756 "] {
            XCTAssertEqual(ConnectionProblem.typedAddress(typed), .pairingCodeAsAddress, typed)
        }
        for address in ["192.168.1.20", "studio-pc", "39075", "3907561", "10.0.0.1", "fe80::1", "12345a"] {
            XCTAssertNil(ConnectionProblem.typedAddress(address), address)
        }
        XCTAssertEqual(ConnectionProblem.pairingCodeAsAddress.actions, [])
        XCTAssertTrue(ConnectionProblem.pairingCodeAsAddress.detail.contains("192.168.1.20"))
    }

    func testNoReplyFromAHomeRoutersAddressSaysItIsTheRouter() {
        let noReply = { (host: String, usb: Bool) in
            ConnectionProblem.timeout(listenerReady: true, helloAttempts: 6, handshakeDone: false,
                                      datagramsReceived: 0, isUSB: usb, host: host)
        }
        for router in ["192.168.1.1", "192.168.0.1", "10.0.0.1", "172.16.4.1", "172.31.255.1"] {
            XCTAssertEqual(noReply(router, false), .routerAddress, router)
        }
        // Other private addresses, public and documentation addresses, and
        // names keep the general advice. So does the cable.
        for other in ["192.168.1.20", "192.168.1.10", "172.32.0.1", "8.8.8.1", "192.0.2.1", "100.64.0.1", "studio-pc", "390756"] {
            XCTAssertEqual(noReply(other, false), .noReply(usb: false), other)
        }
        XCTAssertEqual(noReply("USB", true), .noReply(usb: true))
        // A router address that answered is a PC after all.
        XCTAssertEqual(ConnectionProblem.timeout(listenerReady: true, helloAttempts: 2, handshakeDone: true,
                                                 datagramsReceived: 0, isUSB: false, host: "192.168.1.1"), .noPicture)
        XCTAssertEqual(ConnectionProblem.routerAddress.actions, [.retry])
    }

    func testTheSupportLogSaysAnAcceptedSessionSentNoPicture() {
        let debug = ConnectionDebugState(host: "192.168.1.20", port: 9876, listenerReadyPort: 9877, helloAttempts: 1)
        let accepted = debug.timeoutSummary(handshakeDone: true)
        XCTAssertFalse(accepted.contains("no datagrams came back"), accepted)
        XCTAssertTrue(accepted.contains("Session accepted") && accepted.contains("no video"), accepted)
        XCTAssertTrue(debug.timeoutSummary(handshakeDone: false).contains("no datagrams came back"))
    }

    func testLinkFailuresBecomePlainProblemsForTheirTransport() {
        let wifi = { (error: LinkError) in ConnectionProblem.linkFailure(error, isUSB: false, host: "studio-pc") }
        // DNS: a name that doesn't resolve, through either socket backend.
        let noSuchRecord = LinkError(.pathUnavailable,
            "UDP path not ready: The operation couldn’t be completed. (Network.NWError error -65554 - NoSuchRecord)",
            underlying: NWError.dns(-65554))
        XCTAssertEqual(wifi(noSuchRecord), .badAddress("studio-pc"))
        XCTAssertEqual(wifi(LinkError(.resolve, "Cannot resolve studio-pc: nodename nor servname provided")), .badAddress("studio-pc"))
        // Permission: Local Network access off, reported either way.
        XCTAssertEqual(wifi(LinkError(.localNetworkDenied, "UDP path not ready: denied", underlying: NWError.posix(.EPERM))), .localNetworkBlocked)
        XCTAssertEqual(wifi(LinkError(.connectionFailed, "UDP connection failed: PolicyDenied", underlying: NWError.dns(-65570))), .localNetworkBlocked)
        // Offline.
        XCTAssertEqual(wifi(LinkError(.pathUnavailable, "UDP path not ready: Network is down", underlying: NWError.posix(.ENETDOWN))), .noNetwork)
        // Unknown failures get general advice, never the raw error.
        let unknown = LinkError(.connectionFailed, "UDP socket failed: Address already in use", underlying: POSIXError(.EADDRINUSE))
        XCTAssertEqual(wifi(unknown), .couldNotConnect)
        XCTAssertFalse(ConnectionProblem.couldNotConnect.detail.contains("Address already in use"))
        // USB: a cable failure is never a Wi-Fi or DNS problem.
        let usbReset = LinkError(.receive, "USB receive failed: The operation couldn’t be completed. (Network.NWError error 54 - Connection reset by peer)",
            underlying: NWError.posix(.ECONNRESET))
        for error in [usbReset, noSuchRecord, LinkError(.timeout, "USB handshake timed out.")] {
            let problem = ConnectionProblem.linkFailure(error, isUSB: true, host: "USB")
            XCTAssertEqual(problem, .usbFailed, error.message)
        }
        let usb = ConnectionProblem.usbFailed
        XCTAssertTrue(usb.detail.contains("cable") && usb.detail.contains("Apple Devices"), usb.detail)
        XCTAssertFalse(usb.detail.contains("Wi-Fi"), usb.detail)
        XCTAssertEqual(usb.actions, [.retry])
    }

    /// No route to the PC, or to its network, while this iPad's own network
    /// works: a PC that is asleep or on another network. Not an offline iPad.
    func testAnUnreachablePCIsNotAnOfflineIPad() {
        let wifi = { (error: LinkError) in ConnectionProblem.linkFailure(error, isUSB: false, host: "studio-pc") }
        for code: POSIXErrorCode in [.EHOSTUNREACH, .ENETUNREACH] {
            XCTAssertEqual(wifi(LinkError(.connectionFailed, "UDP connection failed",
                underlying: NWError.posix(code))), .couldNotConnect, "\(code)")
            XCTAssertEqual(wifi(LinkError(.receive, "UDP receive error",
                underlying: POSIXError(code))), .couldNotConnect, "\(code)")
        }
    }

    func testGivingUpOnReconnectingKeepsASpecificLastFailure() {
        // A classified failure in the last attempt is more useful than
        // "Couldn't reconnect"; a PC that just never answered is not.
        for specific: ConnectionProblem in [.localNetworkBlocked, .noNetwork, .badAddress("pc"), .usbFailed, .noPicture,
                                             .pcBusy, .pcEndedSession] {
            XCTAssertEqual(ConnectionProblem.afterReconnecting(lastAttempt: specific), specific)
        }
        for generic: ConnectionProblem? in [nil, .noReply(usb: false), .reconnecting(attempt: 6, of: 6), .couldNotConnect] {
            XCTAssertEqual(ConnectionProblem.afterReconnecting(lastAttempt: generic), .reconnectFailed)
        }
    }

    func testEveryProblemSaysWhatToDoWithoutTransportJargon() {
        let all: [ConnectionProblem] = [
            .noReply(usb: false), .noReply(usb: true), .noPicture, .receiverNotReady, .badAddress("pc"),
            .noNetwork, .localNetworkBlocked, .couldNotConnect, .usbFailed, .lost(.pcStoppedResponding), .lost(.cableUnplugged),
            .lost(.usbTurnedOff), .reconnecting(attempt: 2, of: 6), .reconnectFailed, .pcEndedSession,
            .pcBusy, .pairingCodeAsAddress, .routerAddress,
            .pcUpdating(.reconnecting), .pcUpdating(.manual), .pcUpdating(.notBack),
        ]
        for problem in all {
            XCTAssertFalse(problem.title.isEmpty)
            XCTAssertFalse(problem.detail.isEmpty)
            for jargon in ["HELLO", "datagram", "heartbeat", "host", "UDP", "VideoToolbox", "FramePacket", "NWError", "POSIX", "NoSuchRecord"] {
                XCTAssertFalse(problem.title.contains(jargon) || problem.detail.contains(jargon), "\(problem): \(jargon)")
            }
        }
        XCTAssertEqual(ConnectionProblem.noReply(usb: false).actions, [.retry])
        XCTAssertEqual(ConnectionProblem.localNetworkBlocked.actions, [.openSettings])
        XCTAssertEqual(ConnectionProblem.lost(.cableUnplugged).actions, [])
        XCTAssertTrue(ConnectionProblem.noReply(usb: false).detail.contains("Firewall"))
        XCTAssertTrue(ConnectionProblem.noReply(usb: true).detail.contains("Apple Devices"))
    }

    func testAnUpdateStaysTheReasonWhileThePCRestarts() {
        // Busy during the handoff and silence while Setup runs are the update.
        for attempt: ConnectionProblem in [.pcBusy, .noReply(usb: false), .pcEndedSession] {
            XCTAssertEqual(ConnectionProblem.kept(.pcUpdating(.reconnecting), after: attempt), .pcUpdating(.reconnecting), "\(attempt)")
        }
        // Something the user must fix replaces it.
        XCTAssertEqual(ConnectionProblem.kept(.pcUpdating(.reconnecting), after: .localNetworkBlocked), .localNetworkBlocked)
        // Unchanged rules otherwise.
        XCTAssertEqual(ConnectionProblem.kept(nil, after: .pcBusy), .pcBusy)
        XCTAssertEqual(ConnectionProblem.kept(.usbFailed, after: .noReply(usb: false)), .usbFailed)
        XCTAssertEqual(ConnectionProblem.afterReconnecting(lastAttempt: .pcUpdating(.reconnecting)), .pcUpdating(.reconnecting))
        XCTAssertEqual(ConnectionProblem.pcUpdating(.reconnecting).actions, [.retry])
    }

    func testTheUpdateMessagePromisesOnlyWhatWillHappen() {
        let reconnecting = ConnectionProblem.pcUpdating(.reconnecting)
        XCTAssertTrue(reconnecting.detail.contains("reconnects when it's back"), reconnecting.detail)
        // Auto-reconnect off: nothing reconnects by itself.
        let manual = reconnecting.withoutReconnecting
        XCTAssertEqual(manual, .pcUpdating(.manual))
        XCTAssertFalse(manual.detail.contains("reconnects"), manual.detail)
        XCTAssertTrue(manual.detail.contains("Try again") && manual.detail.contains("opened again"), manual.detail)
        XCTAssertEqual(manual.actions, [.retry])
        // Every attempt ran out.
        let notBack = reconnecting.afterTheCycle
        XCTAssertEqual(notBack, .pcUpdating(.notBack))
        XCTAssertFalse(notBack.detail.contains("reconnects"), notBack.detail)
        XCTAssertTrue(notBack.title.contains("hasn't come back"), notBack.title)
        XCTAssertEqual(notBack.actions, [.retry])
        // Other problems pass through both unchanged.
        XCTAssertEqual(ConnectionProblem.pcBusy.withoutReconnecting, .pcBusy)
        XCTAssertEqual(ConnectionProblem.reconnectFailed.afterTheCycle, .reconnectFailed)
        XCTAssertEqual(ConnectionProblem.lost(.pcStoppedResponding).withoutReconnecting, .lost(.pcStoppedResponding))
    }

    func testUSBCardTextIsPlain() {
        XCTAssertEqual(USBStatusText.describe("USB: off"), "Off. Turn on to connect with a cable.")
        XCTAssertEqual(USBStatusText.describe("USB: connected to Studio PC"), "Connected to Studio PC over the cable.")
        XCTAssertTrue(USBStatusText.describe("USB: waiting for a cable").contains("Apple Devices"))
        XCTAssertTrue(USBStatusText.describe("USB: disconnected").contains("Connect USB"))
        for status in ["USB: off", "USB: waiting for a cable", "USB: disconnected", "USB: connecting", "USB: unavailable", "USB: connected to PC"] {
            XCTAssertFalse(USBStatusText.describe(status).hasPrefix("USB:"), status)
        }
    }
}
