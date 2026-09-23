import XCTest
@testable import EternalMonitor

private final class MemoryPairingStorage: PairingStorage {
    var values: [String: Data] = [:]
    func records() throws -> [PairingRecord] { values.map { PairingRecord(account: $0.key, token: $0.value) } }
    func save(account: String, token: Data) throws { values[account] = token }
    func remove(account: String?) throws {
        if let account { values.removeValue(forKey: account) } else { values.removeAll() }
    }
}

final class PairingTests: XCTestCase {
    @MainActor func testStoreUsesHostNameAndAddressAndPromotesQRIdentity() async throws {
        let backend = MemoryPairingStorage()
        let store = PairingStore(storage: backend)
        let address = PairingStore.address(host: "PC.local", port: 9876)
        let token = Data(1...16)
        XCTAssertEqual(address, "pc.local:9876")
        try store.save(token: token, hostName: "", address: address)
        XCTAssertEqual(backend.values["|pc.local:9876"], token)
        XCTAssertTrue(store.isPaired(host: "PC.local", port: 9876))
        try store.save(token: token, hostName: "My PC", address: address)
        XCTAssertNil(backend.values["|pc.local:9876"])
        XCTAssertEqual(backend.values["My PC|pc.local:9876"], token)
        XCTAssertEqual(try store.token(address: address), token)
        XCTAssertFalse(store.isPaired(host: "PC.local", port: 9877))
        XCTAssertEqual(PairingStore.address(host: "fe80::1", port: 9876), "[fe80::1]:9876")
    }

    @MainActor func testRevocationIsPerAddressAndForgetAllRemovesEveryHost() async throws {
        let backend = MemoryPairingStorage()
        let store = PairingStore(storage: backend)
        try store.save(token: Data(1...16), hostName: "PC", address: "192.0.2.1:9876")
        try store.save(token: Data(17...32), hostName: "PC", address: "192.0.2.2:9876")
        try store.remove(address: "192.0.2.1:9876")
        XCTAssertNil(try store.token(address: "192.0.2.1:9876"))
        XCTAssertEqual(try store.token(address: "192.0.2.2:9876"), Data(17...32))
        try store.forgetAll()
        XCTAssertTrue(backend.values.isEmpty)
    }

    @MainActor func testMissingMalformedAndZeroTokensAreNotPairings() async throws {
        let backend = MemoryPairingStorage()
        let store = PairingStore(storage: backend)
        for invalid in [Data(), Data(repeating: 0, count: 16), Data(repeating: 1, count: 15)] {
            try store.save(token: invalid, hostName: "PC", address: "pc:9876")
            XCTAssertTrue(backend.values.isEmpty)
            backend.values["PC|pc:9876"] = invalid
            XCTAssertNil(try store.token(address: "pc:9876"))
            backend.values.removeAll()
        }
        XCTAssertNil(try store.token(address: "missing:9876"))
    }

    @MainActor func testSheetValidationWrongCodeRetryAndLeadingZero() async {
        let model = PairingSheetModel()
        model.reject(.unauthorized)
        XCTAssertNil(model.error)
        for invalid in ["12345", "1234567", "١٢٣٤٥٦", "12 456", "abcdef"] {
            model.code = invalid
            XCTAssertNil(model.submit())
            XCTAssertFalse(model.submitting)
        }
        model.code = "001234"
        XCTAssertEqual(model.submit(), 1234)
        XCTAssertTrue(model.submitting)
        XCTAssertNil(model.submit())
        model.reject(.unauthorized)
        XCTAssertTrue(model.error?.contains("didn’t match") == true)
        XCTAssertFalse(model.submitting)
        model.code = "567890"
        XCTAssertEqual(model.submit(), 567890)
        XCTAssertNil(model.error)
        model.timedOut()
        XCTAssertFalse(model.submitting)
        XCTAssertTrue(model.error?.contains("did not respond") == true)
    }

    @MainActor func testRateLimitedSheetWaitsFullMinute() async {
        let model = PairingSheetModel()
        let now = Date(timeIntervalSince1970: 1000)
        model.code = "123456"
        model.reject(.rateLimited, now: now)
        XCTAssertEqual(model.remaining(at: now), 60)
        XCTAssertEqual(model.remaining(at: now.addingTimeInterval(59.1)), 1)
        XCTAssertNil(model.submit(now: now.addingTimeInterval(59.9)))
        XCTAssertEqual(model.submit(now: now.addingTimeInterval(60)), 123456)
    }

    func testQRTokenAndLegacyLinks() throws {
        let target = try PairingCode.parse("eternaldisplay://[fe80::1]:9876?t=0102030405060708090a0b0c0d0e0f10").get()
        XCTAssertEqual(target.host, "fe80::1")
        XCTAssertEqual(target.token, Data(1...16))
        XCTAssertNil(try PairingCode.parse("eternaldisplay://pc:9876").get().token)
        for query in ["t=bad", "t=", "t=00000000000000000000000000000000", "t=0102030405060708090a0b0c0d0e0f10&t=0102030405060708090a0b0c0d0e0f10"] {
            guard case .failure(.invalidToken) = PairingCode.parse("eternaldisplay://pc:9876?\(query)") else {
                return XCTFail("Accepted invalid token query")
            }
        }
    }
}

final class PairingControlTests: XCTestCase {
    func testTokenHandshakeCodeRetryAndStaleAckIsolation() async throws {
        let queue = DispatchQueue(label: "pairing-control-test")
        let (stream, continuation) = AsyncStream<Data>.makeStream()
        let channel = ControlChannel(queue: queue) { continuation.yield($0) }
        defer { channel.stop(); continuation.finish() }
        let identity = ControlChannel.ClientIdentity(deviceName: "Test iPad", screenPxW: 640, screenPxH: 360,
            screenPtW: 640, screenPtH: 360, refreshHz: 60, authToken: Data(1...16))
        channel.startHandshake(listenPort: 50000, identity: identity)
        var messages = stream.makeAsyncIterator()
        let firstData = await messages.next()
        guard case .hello2(let first) = try XCTUnwrap(Wire.parseControl(XCTUnwrap(firstData))).message else {
            return XCTFail("missing token handshake")
        }
        XCTAssertEqual(first.authToken, Data(1...16))
        func ack(_ nonce: UInt32, _ status: HelloStatus) -> Data {
            Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .helloAck(HelloAck(
                status: status, acceptedVersion: 2, clientNonce: nonce, sessionId: status == .ok ? 41 : 0,
                heartbeatIntervalMs: 1000, reportIntervalMs: 500, livenessTimeoutMs: 3000,
                streamConfig: StreamConfig(), hostName: "Test PC", authToken: Data(17...32), hostCaps: 0)))
        }
        queue.sync { channel.handleControl(ack(first.clientNonce, .unauthorized)) }
        channel.retryPairing(code: 123456)
        var submitted: Hello2?
        while let data = await messages.next() {
            if case .hello2(let hello) = Wire.parseControl(data)?.message, hello.pairingCode == 123456 {
                submitted = hello; break
            }
        }
        let second = try XCTUnwrap(submitted)
        XCTAssertNotEqual(second.clientNonce, first.clientNonce)
        XCTAssertEqual(second.authToken, Data(repeating: 0, count: 16))
        queue.sync { channel.handleControl(ack(first.clientNonce, .ok)) }
        XCTAssertEqual(channel.currentSessionId, 0)
        queue.sync { channel.handleControl(ack(second.clientNonce, .ok)) }
        XCTAssertEqual(channel.currentSessionId, 41)
    }
}
