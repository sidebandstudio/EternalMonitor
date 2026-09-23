import XCTest
import Network
import os
@testable import EternalMonitor

final class USBListenerTests: XCTestCase {
    func testLoopbackAcceptAndControlRoundTripAcrossBytewiseWrites() async throws {
        let listener = USBListener(port: 0)
        let ready = expectation(description: "loopback listener ready")
        let accepted = expectation(description: "valid preamble accepted")
        let echoed = expectation(description: "framed control round trip")
        let port = OSAllocatedUnfairLock<UInt16>(initialState: 0)
        let active = OSAllocatedUnfairLock<FramedLink?>(initialState: nil)
        let ping = Wire.encodeControl(sessionId: 1, msgSeq: 1, message: .ping(WirePing(t1Us: 42)))
        let framed = try EMLinkFramer.encode(ping)
        listener.onState = { state in
            if case .listening(let value) = state { port.withLock { $0 = value }; ready.fulfill() }
        }
        listener.onAccepted = { link in
            active.withLock { $0 = link }
            link.onControlDatagram = { [weak link] packet in
                XCTAssertEqual(packet, ping)
                link?.send(packet)
            }
            XCTAssertTrue(link.start(host: "USB"))
            accepted.fulfill()
        }
        listener.start()
        defer { active.withLock { $0 }?.stop(); listener.stop() }
        await fulfillment(of: [ready], timeout: 3)
        let client = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port.withLock { $0 })!, using: .tcp)
        defer { client.cancel() }
        client.stateUpdateHandler = { state in
            guard case .ready = state else { return }
            for byte in EMLinkFramer.preamble + framed {
                client.send(content: Data([byte]), completion: .contentProcessed { XCTAssertNil($0) })
            }
            client.receive(minimumIncompleteLength: framed.count, maximumLength: framed.count) { data, _, _, error in
                XCTAssertNil(error)
                XCTAssertEqual(data, framed)
                echoed.fulfill()
            }
        }
        client.start(queue: DispatchQueue(label: "test.usb.client"))
        await fulfillment(of: [accepted, echoed], timeout: 3)
        client.stateUpdateHandler = nil
    }

    func testInvalidAndMissingPreamblesCloseWithoutAccepting() async throws {
        for badPreamble in [Data("BADBYTES".utf8), Data()] {
            let listener = USBListener(port: 0)
            let ready = expectation(description: "listener ready")
            let closed = expectation(description: "invalid peer closed")
            let accepted = expectation(description: "invalid peer never accepted")
            accepted.isInverted = true
            let port = OSAllocatedUnfairLock<UInt16>(initialState: 0)
            listener.onState = { state in
                if case .listening(let value) = state { port.withLock { $0 = value }; ready.fulfill() }
            }
            listener.onAccepted = { _ in accepted.fulfill() }
            listener.start()
            await fulfillment(of: [ready], timeout: 3)
            let client = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port.withLock { $0 })!, using: .tcp)
            client.stateUpdateHandler = { state in
                guard case .ready = state else { return }
                if !badPreamble.isEmpty { client.send(content: badPreamble, completion: .contentProcessed { XCTAssertNil($0) }) }
                client.receive(minimumIncompleteLength: 1, maximumLength: 1) { data, _, complete, error in
                    XCTAssertTrue(complete || error != nil)
                    XCTAssertTrue(data == nil || data?.isEmpty == true)
                    closed.fulfill()
                }
            }
            client.start(queue: DispatchQueue(label: "test.usb.invalid"))
            await fulfillment(of: [closed], timeout: 3)
            await fulfillment(of: [accepted], timeout: 0.05)
            client.stateUpdateHandler = nil
            client.cancel()
            listener.stop()
        }
    }
}
