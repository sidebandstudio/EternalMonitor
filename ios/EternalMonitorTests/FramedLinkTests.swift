import Darwin
import Network
import XCTest
@testable import EternalMonitor

final class FramedLinkTests: XCTestCase {
    func testStopFlushesQueuedControlsAndGoodbyeInOrder() throws {
        let server = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP)
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
        XCTAssertEqual(listen(server, 1), 0)
        var timeout = timeval(tv_sec: 2, tv_usec: 0)
        XCTAssertEqual(setsockopt(server, SOL_SOCKET, SO_RCVTIMEO, &timeout,
            socklen_t(MemoryLayout<timeval>.size)), 0)
        let connection = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        let link = FramedLink(connection: connection)
        defer { link.stop() }
        var packets = (1...16).map { sequence in
            Wire.encodeControl(sessionId: 7, msgSeq: UInt32(sequence), message: .ping(WirePing(t1Us: 42)))
        }
        packets.append(Wire.encodeControl(sessionId: 7, msgSeq: 17, message: .bye(.appBackground)))
        let expected = try packets.reduce(into: Data()) { $0.append(try EMLinkFramer.encode($1)) }
        let stopped = expectation(description: "Controls queued and link stopped")
        link.onPreambleReady = { [weak link] in
            guard let link else { return }
            XCTAssertTrue(link.start(host: "127.0.0.1"))
            for packet in packets { link.send(packet) }
            link.stop()
            stopped.fulfill()
        }
        link.validatePreamble()
        let peer = accept(server, nil, nil)
        XCTAssertGreaterThanOrEqual(peer, 0)
        guard peer >= 0 else { return }
        defer { Darwin.close(peer) }
        XCTAssertEqual(setsockopt(peer, SOL_SOCKET, SO_RCVTIMEO, &timeout,
            socklen_t(MemoryLayout<timeval>.size)), 0)
        let count = EMLinkFramer.preamble.withUnsafeBytes { Darwin.send(peer, $0.baseAddress, $0.count, 0) }
        XCTAssertEqual(count, EMLinkFramer.preamble.count)
        wait(for: [stopped], timeout: 3)
        var actual = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)
        while actual.count < expected.count {
            let count = recv(peer, &buffer, buffer.count, 0)
            guard count > 0 else { break }
            actual.append(contentsOf: buffer.prefix(count))
        }
        XCTAssertEqual(actual, expected, "Stop must deliver all queued controls before the final BYE")
    }
}
