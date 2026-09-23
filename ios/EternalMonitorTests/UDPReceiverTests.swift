import Darwin
import XCTest
@testable import EternalMonitor

final class UDPReceiverTests: XCTestCase {
    func testNetworkFrameworkSendsGoodbyeBeforeImmediateStop() throws {
        let server = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
        XCTAssertGreaterThanOrEqual(server, 0)
        defer { Darwin.close(server) }
        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        var size = socklen_t(MemoryLayout<sockaddr_in>.size)
        let bindSize = size
        let port = withUnsafeMutablePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                XCTAssertEqual(Darwin.bind(server, $0, bindSize), 0)
                XCTAssertEqual(getsockname(server, $0, &size), 0)
            }
            return UInt16(bigEndian: pointer.pointee.sin_port)
        }
        var timeout = timeval(tv_sec: 2, tv_usec: 0)
        XCTAssertEqual(setsockopt(server, SOL_SOCKET, SO_RCVTIMEO, &timeout,
            socklen_t(MemoryLayout<timeval>.size)), 0)
        for sequence: UInt32 in 1...8 {
            let receiver = UDPReceiver(port: port, backend: .networkFramework)
            let ready = expectation(description: "Goodbye queued before stop")
            let goodbye = Wire.encodeControl(sessionId: 7, msgSeq: sequence, message: .bye(.appBackground))
            receiver.onListenerReady = { [weak receiver] _ in
                receiver?.send(goodbye)
                receiver?.stop()
                ready.fulfill()
            }
            XCTAssertTrue(receiver.start(host: "127.0.0.1"))
            wait(for: [ready], timeout: 3)
            var buffer = [UInt8](repeating: 0, count: 2048)
            let count = recv(server, &buffer, buffer.count, 0)
            XCTAssertEqual(count, goodbye.count, "Stopping must not cancel the queued BYE")
            guard count == goodbye.count else { return }
            XCTAssertEqual(Data(buffer.prefix(count)), goodbye)
        }
    }

    func testBSDLoopbackDrainsBurstAndReportsKernelBuffer() throws {
        try exerciseLoopback(family: AF_INET, host: "127.0.0.1")
    }

    func testBSDIPv6Loopback() throws {
        try exerciseLoopback(family: AF_INET6, host: "::1")
    }

    private func exerciseLoopback(family: Int32, host: String) throws {
        let server = socket(family, SOCK_DGRAM, IPPROTO_UDP)
        XCTAssertGreaterThanOrEqual(server, 0)
        defer { Darwin.close(server) }
        var address = sockaddr_storage()
        address.ss_family = sa_family_t(family)
        address.ss_len = UInt8(family == AF_INET6 ? MemoryLayout<sockaddr_in6>.size : MemoryLayout<sockaddr_in>.size)
        var addressSize = socklen_t(address.ss_len)
        let bindSize = addressSize
        let status = withUnsafeMutablePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(server, $0, bindSize)
            }
        }
        XCTAssertEqual(status, 0)
        let port: UInt16 = withUnsafeMutablePointer(to: &address) { pointer in
            let result = pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getsockname(server, $0, &addressSize)
            }
            XCTAssertEqual(result, 0)
            if family == AF_INET6 {
                return pointer.withMemoryRebound(to: sockaddr_in6.self, capacity: 1) { UInt16(bigEndian: $0.pointee.sin6_port) }
            }
            return pointer.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt16(bigEndian: $0.pointee.sin_port) }
        }
        var timeout = timeval(tv_sec: 2, tv_usec: 0)
        XCTAssertEqual(setsockopt(server, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size)), 0)
        let receiver = UDPReceiver(port: port, backend: .bsd)
        defer { receiver.stop() }
        let ready = expectation(description: "Ephemeral socket bound")
        let received = expectation(description: "Entire UDP burst drained")
        received.expectedFulfillmentCount = 64
        let packet = Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .ping(WirePing(t1Us: 123)))
        receiver.onListenerReady = { localPort in
            XCTAssertGreaterThan(localPort, 0)
            receiver.send(packet)
            ready.fulfill()
        }
        // Avoid callback retaining the receiver after the assertions.
        defer { receiver.onListenerReady = nil }
        receiver.onError = { XCTFail($0) }
        receiver.onControlDatagram = { data in
            XCTAssertEqual(data, packet)
            received.fulfill()
        }
        XCTAssertTrue(receiver.start(host: host))
        wait(for: [ready], timeout: 3)
        XCTAssertNotNil(receiver.localPort())
        let actualBuffer = receiver.receiveBufferBytes
        print("BSD_TEST family=\(family) SO_RCVBUF=\(actualBuffer) requested=4194304")
        #if targetEnvironment(simulator)
        // Darwin returns the actual capacity, including any host-kernel clamp.
        XCTAssertGreaterThanOrEqual(actualBuffer, 1_048_576)
        XCTAssertLessThanOrEqual(actualBuffer, 4_194_304)
        #else
        XCTAssertGreaterThanOrEqual(actualBuffer, 1_048_576)
        #endif
        var peer = sockaddr_storage()
        var peerSize = socklen_t(MemoryLayout<sockaddr_storage>.size)
        var buffer = [UInt8](repeating: 0, count: 2048)
        let count = withUnsafeMutablePointer(to: &peer) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                recvfrom(server, &buffer, buffer.count, 0, $0, &peerSize)
            }
        }
        XCTAssertEqual(count, packet.count)
        guard count == packet.count else { return }
        XCTAssertEqual(Data(buffer.prefix(count)), packet)
        for _ in 0..<64 {
            let sent = packet.withUnsafeBytes { bytes in
                withUnsafePointer(to: &peer) { pointer in
                    pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                        sendto(server, bytes.baseAddress, bytes.count, 0, $0, peerSize)
                    }
                }
            }
            XCTAssertEqual(sent, packet.count)
        }
        wait(for: [received], timeout: 3)
        receiver.stop()
        XCTAssertNil(receiver.localPort())
        XCTAssertEqual(receiver.receiveBufferBytes, 0)
    }

    func testInvalidPortDoesNotStart() {
        let receiver = UDPReceiver(port: 0, backend: .bsd)
        XCTAssertFalse(receiver.start(host: "127.0.0.1"))
    }
}
