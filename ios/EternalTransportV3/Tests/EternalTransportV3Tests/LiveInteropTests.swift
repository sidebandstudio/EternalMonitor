import Foundation
import XCTest
@testable import EternalTransportV3

/// The Swift client against the Rust reference host (`HostSessions`, in
/// `proto/examples/v3_interop_host.rs`) over real UDP on loopback.
/// `scripts/v3_swift_interop.sh` starts the host and passes its port and
/// test keys in `EM_V3_INTEROP_*`; without them the test is skipped.
///
/// One run covers: a clean connect with the slot status; sealed records
/// both ways; a reconnect that supersedes the live session and drains the
/// old one; `Stale` and `TooNew` answers and their retries; a refusal with
/// the key's tombstone; a sealed BYE. The host's own count of what it saw
/// is read back over a sealed record at the end.
final class LiveInteropTests: XCTestCase {
    /// The harness's query records (`QUERY` in the host).
    static let query: UInt8 = 0x3F

    struct Setup {
        let port: UInt16
        let hostId: Id16
        let a: ClientKey
        let b: ClientKey
    }

    func setup() throws -> Setup {
        let env = ProcessInfo.processInfo.environment
        guard let port = env["EM_V3_INTEROP_PORT"].flatMap(UInt16.init) else {
            throw XCTSkip("no interop host: run scripts/v3_swift_interop.sh")
        }
        func value(_ name: String) throws -> [UInt8] {
            try unhex(try XCTUnwrap(env["EM_V3_INTEROP_\(name)"], name))
        }
        func key(_ prefix: String) throws -> ClientKey {
            try XCTUnwrap(ClientKey(devId: Id16(try value("\(prefix)_DEV_ID"))!, kDev: SecretBytes(copying: try value("\(prefix)_K_DEV"))))
        }
        return Setup(port: port, hostId: try XCTUnwrap(Id16(try value("HOST_ID"))), a: try key("A"), b: try key("B"))
    }

    func testTheSwiftClientAgainstTheRustHost() throws {
        let setup = try setup()
        let start = ContinuousClock.now
        let wallUs = { UInt64(Date().timeIntervalSince1970 * 1_000_000) }
        var hello = HelloPayload()
        hello.deviceName = "Interop iPad"
        hello.decoderCaps = 3

        // 1. A clean connect under key a.
        let one = try Peer(setup, hello: hello, start: start)
        _ = one.run(one.client.connect(now: one.now, key: setup.a, clockUs: wallUs()))
        XCTAssertNotNil(one.pump { $0.liveSessions.count == 1 && $0.statuses.count == 1 })
        let session1 = try XCTUnwrap(one.liveSessions.first)
        XCTAssertEqual(one.accepts.first?.hostName, "INTEROP-HOST")
        XCTAssertEqual(one.accepts.first?.livenessTimeoutMs, 3000)
        XCTAssertEqual(one.events.first, .releaseAllInput)
        let status = try XCTUnwrap(one.statuses.first)
        XCTAssertEqual(status.state(of: setup.a.devId), .active)
        XCTAssertEqual(status.state(of: setup.b.devId), .pending)

        // 2. Sealed records both ways: inputs echoed back, heartbeats.
        for index in 0..<50 {
            _ = one.run([.send(try XCTUnwrap(one.client.seal(now: one.now, innerType: V2.inputEventType, body: [UInt8(index), 0xEE])))])
        }
        XCTAssertNotNil(one.pump { $0.delivered(V2.inputEventType).count == 50 && !$0.delivered(V2.heartbeatType).isEmpty })
        XCTAssertEqual(one.delivered(V2.inputEventType).map(\.body), (0..<50).map { [UInt8($0), 0xEE] })
        let counters = one.delivered(V2.inputEventType).map(\.counter) + one.delivered(V2.heartbeatType).map(\.counter)
        XCTAssertEqual(Set(counters).count, counters.count, "a fresh counter per record")

        // 3. A reconnect: the new session supersedes the old one without a
        // BYE, and the old one drains.
        _ = one.run(one.client.connect(now: one.now, key: setup.a, clockUs: wallUs()))
        XCTAssertNotNil(one.pump { $0.liveSessions.count == 2 })
        let session2 = one.liveSessions[1]
        XCTAssertNotEqual(session2, session1)
        XCTAssertEqual(one.client.drainingSession, session1)
        XCTAssertNotNil(one.pump { !$0.delivered(V2.heartbeatType, session: session2).isEmpty })
        XCTAssertEqual(one.client.firstIdrDecoded(sessionId: session2), [.closed(sessionId: session1, reason: .drainEnded)])
        XCTAssertFalse(one.events.contains { if case .closed(session1, .hostBye) = $0 { return true } else { return false } },
                       "no BYE on supersede")

        // 4. Stale: another client under a with a clock far behind.
        let stale = try Peer(setup, hello: hello, start: start)
        _ = stale.run(stale.client.connect(now: stale.now, key: setup.a, clockUs: 1_000))
        XCTAssertNotNil(stale.pump { !$0.liveSessions.isEmpty })
        XCTAssertGreaterThan(stale.sentCount, 2, "message 1, the retry after Stale, a confirm")
        XCTAssertGreaterThan(stale.client.lastTimestamp, one.client.lastTimestamp, "jumped past the committed value")

        // 5. TooNew: a clock two days ahead of the host's.
        let ahead = try Peer(setup, hello: hello, start: start)
        let twoDays: UInt64 = 2 * 24 * 3600 * 1_000_000
        _ = ahead.run(ahead.client.connect(now: ahead.now, key: setup.a, clockUs: wallUs() + twoDays))
        XCTAssertNotNil(ahead.pump { !$0.liveSessions.isEmpty })
        XCTAssertLessThan(ahead.client.lastTimestamp, wallUs() + twoDays - 3_600_000_000, "pulled back to the host's limit")

        // 6. A refusal: key b's Finalize always conflicts on this host.
        let refused = try Peer(setup, hello: hello, start: start)
        _ = refused.run(refused.client.connect(now: refused.now, key: setup.b, clockUs: wallUs()))
        XCTAssertNotNil(refused.pump { $0.events.contains { if case .refused = $0 { return true } else { return false } } })
        guard case .refused(let devId, let refusal)? = refused.events.first(where: { if case .refused = $0 { return true } else { return false } }) else {
            return XCTFail("no refusal")
        }
        XCTAssertEqual(devId, setup.b.devId)
        XCTAssertEqual(refusal.tombstones, [setup.b.devId])
        XCTAssertNil(refused.client.liveSession)
        var ring = KeyRing(hostId: setup.hostId, slotId: id16(0x5C))
        XCTAssertEqual(ring.add(setup.a), .added)
        XCTAssertEqual(ring.add(setup.b), .added)
        XCTAssertEqual(ring.apply(refusal), [setup.b.devId], "the key rule deletes the refused key")
        XCTAssertEqual(ring.connectOrder().map(\.devId), [setup.a.devId])

        // 7. The host's view so far, then a sealed BYE.
        let summary = try ahead.query("summary")
        XCTAssertEqual(summary["commits"] as? Int, 4)
        XCTAssertEqual(summary["acks"] as? Int, 4)
        XCTAssertEqual(summary["refused"] as? Int, 1)
        XCTAssertGreaterThanOrEqual(summary["delivered"] as? Int ?? 0, 50)
        XCTAssertEqual((summary["closed"] as? [String: Int])?["superseded"], 3)
        let bye = ahead.client.disconnect(reason: 0)
        XCTAssertTrue(bye.contains(.closed(sessionId: ahead.liveSessions[0], reason: .local)))
        _ = ahead.run(bye)

        // 8. One more connect, to read the BYE's effect and stop the host.
        let last = try Peer(setup, hello: hello, start: start)
        last.client.setLastTimestamp(ahead.client.lastTimestamp)
        _ = last.run(last.client.connect(now: last.now, key: setup.a, clockUs: wallUs()))
        XCTAssertNotNil(last.pump { !$0.liveSessions.isEmpty })
        let final = try last.query("quit")
        XCTAssertEqual((final["closed"] as? [String: Int])?["peer_bye"], 1)
        XCTAssertEqual(final["commits"] as? Int, 5)
    }
}

/// One client on its own UDP socket, driven in real time.
final class Peer {
    let client: Client
    private let socket: UDPSocket
    private let start: ContinuousClock.Instant
    private(set) var events: [ClientOutput] = []
    private(set) var sentCount = 0
    private var lastKeepalive: Duration = .zero

    init(_ setup: LiveInteropTests.Setup, hello: HelloPayload, start: ContinuousClock.Instant) throws {
        client = Client(config: ClientConfig(hostId: setup.hostId), hello: hello)
        socket = try UDPSocket(port: setup.port)
        self.start = start
    }

    var now: Duration { ContinuousClock.now - start }

    var liveSessions: [UInt32] {
        events.compactMap { if case .live(let id, _) = $0 { return id } else { return nil } }
    }

    var accepts: [Accept] {
        events.compactMap { if case .live(_, let accept) = $0 { return accept } else { return nil } }
    }

    var statuses: [SlotStatus] {
        events.compactMap { if case .status(_, let status) = $0 { return status } else { return nil } }
    }

    func delivered(_ innerType: UInt8, session: UInt32? = nil) -> [(counter: UInt64, body: [UInt8])] {
        events.compactMap {
            guard case let .deliver(id, counter, inner, body, _) = $0, inner == innerType, session == nil || id == session else { return nil }
            return (counter, body)
        }
    }

    /// Sends what's to be sent and keeps the rest.
    func run(_ outputs: [ClientOutput]) -> [ClientOutput] {
        for output in outputs {
            if case .send(let datagram) = output {
                socket.send(datagram)
                sentCount += 1
            } else {
                events.append(output)
            }
        }
        return outputs
    }

    /// Receives and polls until `done` holds, for at most `timeout`.
    @discardableResult
    func pump(timeout: Duration = .seconds(5), _ done: (Peer) -> Bool) -> Bool? {
        let deadline = now + timeout
        while now < deadline {
            if done(self) { return true }
            let wall = UInt64(Date().timeIntervalSince1970 * 1_000_000)
            if let datagram = socket.receive() {
                _ = run(client.handleDatagram(now: now, datagram: datagram, clockUs: wall))
            }
            _ = run(client.poll(now: now, clockUs: wall))
            // Like the app's reports: keeps the host's 3 s liveness from
            // ending a session the test still uses, however loaded the
            // machine is.
            if client.liveSession != nil, now - lastKeepalive >= .milliseconds(500),
               let ping = client.seal(now: now, innerType: V2.pingType, body: [UInt8](repeating: 0, count: 8)) {
                socket.send(ping)
                lastKeepalive = now
            }
        }
        return done(self) ? true : nil
    }

    /// Asks the host for its summary over a sealed record.
    func query(_ what: String) throws -> [String: Any] {
        let before = delivered(LiveInteropTests.query).count
        _ = run([.send(try XCTUnwrap(client.seal(now: now, innerType: LiveInteropTests.query, body: Array(what.utf8))))])
        XCTAssertNotNil(pump { $0.delivered(LiveInteropTests.query).count > before })
        let body = try XCTUnwrap(delivered(LiveInteropTests.query).last?.body)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(body)) as? [String: Any])
    }
}

/// A connected UDP socket to 127.0.0.1 with a short receive timeout.
final class UDPSocket {
    private let fd: Int32

    init(port: UInt16) throws {
        fd = socket(AF_INET, SOCK_DGRAM, 0)
        guard fd >= 0 else { throw POSIXError(.EIO) }
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard connected == 0 else { throw POSIXError(.ECONNREFUSED) }
        var timeout = timeval(tv_sec: 0, tv_usec: 5_000)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    }

    deinit { close(fd) }

    func send(_ datagram: [UInt8]) {
        datagram.withUnsafeBytes { _ = Darwin.send(fd, $0.baseAddress, $0.count, 0) }
    }

    func receive() -> [UInt8]? {
        var buffer = [UInt8](repeating: 0, count: 2048)
        let count = recv(fd, &buffer, buffer.count, 0)
        return count > 0 ? Array(buffer[..<count]) : nil
    }
}
