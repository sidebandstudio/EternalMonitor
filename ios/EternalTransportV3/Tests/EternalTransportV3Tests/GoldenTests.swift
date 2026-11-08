import CryptoKit
import XCTest
@testable import EternalTransportV3

/// Interop gate: the Swift side reproduces, byte for byte, every vector the
/// Rust reference pins in `proto/testdata/v3` (spec vectors, the session
/// transcript, the iPad-side vectors and the replay scripts). Each test
/// ends by checking it read every entry of its file, so a vector added on
/// the Rust side fails here until Swift checks it too.
final class GoldenTests: XCTestCase {

    // MARK: - noise_vectors.txt

    /// Cacophony's NNpsk0 vector through `SymmetricState` in both roles,
    /// then its transport messages through the record nonce on the raw
    /// `Split()` keys: pins the pattern, HKDF, the nonce format and which
    /// key belongs to which direction.
    func testNoiseNNpsk0Vector() throws {
        let v = try Vectors("noise_vectors.txt")
        let psk = v.secret("nnpsk0.psk")
        let initEphemeral = ephemeral(v["nnpsk0.init_ephemeral"])
        let respEphemeral = ephemeral(v["nnpsk0.resp_ephemeral"])

        var initiator = SymmetricState(protocolName: V3.noiseParams)
        var responder = SymmetricState(protocolName: V3.noiseParams)
        initiator.mixHash(v["nnpsk0.prologue"])
        responder.mixHash(v["nnpsk0.prologue"])

        // -> psk, e
        initiator.mixKeyAndHash(psk)
        let ie = Array(initEphemeral.publicKey.rawRepresentation)
        initiator.mixHash(ie)
        initiator.mixKey(ie)
        let msg0 = ie + initiator.encryptAndHash(v["nnpsk0.msg0.payload"])
        XCTAssertEqual(hex(msg0), hex(v["nnpsk0.msg0.ciphertext"]))
        responder.mixKeyAndHash(psk)
        responder.mixHash(Array(msg0[..<32]))
        responder.mixKey(Array(msg0[..<32]))
        XCTAssertEqual(try responder.decryptAndHash(Array(msg0[32...])), v["nnpsk0.msg0.payload"])

        // <- e, ee
        let re = Array(respEphemeral.publicKey.rawRepresentation)
        responder.mixHash(re)
        responder.mixKey(re)
        try respEphemeral.sharedSecretFromKeyAgreement(with: .init(rawRepresentation: msg0[..<32]))
            .withUnsafeBytes { responder.mixKey($0) }
        let msg1 = re + responder.encryptAndHash(v["nnpsk0.msg1.payload"])
        XCTAssertEqual(hex(msg1), hex(v["nnpsk0.msg1.ciphertext"]))
        initiator.mixHash(Array(msg1[..<32]))
        initiator.mixKey(Array(msg1[..<32]))
        try initEphemeral.sharedSecretFromKeyAgreement(with: .init(rawRepresentation: msg1[..<32]))
            .withUnsafeBytes { initiator.mixKey($0) }
        XCTAssertEqual(try initiator.decryptAndHash(Array(msg1[32...])), v["nnpsk0.msg1.payload"])

        XCTAssertEqual(hex(initiator.handshakeHash), hex(v["nnpsk0.handshake_hash"]))
        XCTAssertEqual(hex(responder.handshakeHash), hex(v["nnpsk0.handshake_hash"]))
        let (i2r, r2i) = initiator.split()
        let (ri2r, rr2i) = responder.split()
        XCTAssertEqual(bytes(i2r), bytes(ri2r))
        XCTAssertEqual(bytes(r2i), bytes(rr2i))

        // msg2..msg5: i->r nonce 0, r->i nonce 0, i->r nonce 1, r->i nonce 1.
        for (index, (key, counter)) in [(i2r, UInt64(0)), (r2i, 0), (i2r, 1), (r2i, 1)].enumerated() {
            let payload = v["nnpsk0.msg\(index + 2).payload"]
            let ciphertext = v["nnpsk0.msg\(index + 2).ciphertext"]
            let box = try ChaChaPoly.seal(payload, using: key.symmetricKey, nonce: noiseNonce(counter), authenticating: [UInt8]())
            XCTAssertEqual(hex(Array(box.ciphertext) + Array(box.tag)), hex(ciphertext), "transport msg\(index + 2)")
            let opened = try ChaChaPoly.open(
                ChaChaPoly.SealedBox(nonce: noiseNonce(counter), ciphertext: ciphertext.dropLast(16), tag: ciphertext.suffix(16)),
                using: key.symmetricKey, authenticating: [UInt8]())
            XCTAssertEqual(Array(opened), payload)
        }

        // RFC 8439 §2.8.2: the ChaCha20-Poly1305 the records use.
        let key = SymmetricKey(data: v["rfc8439.key"])
        let nonce = try ChaChaPoly.Nonce(data: v["rfc8439.nonce"])
        let box = try ChaChaPoly.seal(v["rfc8439.plaintext"], using: key, nonce: nonce, authenticating: v["rfc8439.aad"])
        XCTAssertEqual(hex(Array(box.ciphertext)), hex(v["rfc8439.ciphertext"]))
        XCTAssertEqual(hex(Array(box.tag)), hex(v["rfc8439.tag"]))

        // RFC 7748 §5.2: X25519 iterated from k = u = 9.
        var k = v["rfc7748.start"]
        var u = k
        for round in 1...1000 {
            let shared = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: k)
                .sharedSecretFromKeyAgreement(with: .init(rawRepresentation: u))
            u = k
            k = shared.withUnsafeBytes { Array($0) }
            if round == 1 {
                XCTAssertEqual(hex(k), hex(v["rfc7748.after_1"]))
            }
        }
        XCTAssertEqual(hex(k), hex(v["rfc7748.after_1000"]))
        XCTAssertEqual(v.unused, [])
    }

    // MARK: - session_golden.txt

    static func goldenHello() -> HelloPayload {
        var hello = HelloPayload()
        hello.timestampUs = 1_762_500_000_123_456
        hello.listenPort = 50_123
        hello.decoderCaps = 0x0007
        hello.featureCaps = 0x0017
        hello.screenPxW = 2732
        hello.screenPxH = 2048
        hello.screenPtW = 1366
        hello.screenPtH = 1024
        hello.refreshHz = 120
        hello.preferredFps = 120
        hello.deviceId = 0x0102_0304_0506_0708
        hello.deviceName = "Test iPad"
        return hello
    }

    func testSessionTranscript() throws {
        let v = try Vectors("session_golden.txt")
        // The inputs are the Rust test's `pattern(start, step)` values;
        // checking them here pins this file's hard-coded copies below.
        XCTAssertEqual(v["input.k_dev"], pattern(32, 0x10, 7))
        XCTAssertEqual(v["input.dev_id"], pattern(16, 0xD0, 3))
        XCTAssertEqual(v["input.host_id"], pattern(16, 0xB0, 5))
        XCTAssertEqual(v["input.init_ephemeral"], pattern(32, 0x21, 11))
        XCTAssertEqual(v["input.resp_ephemeral"], pattern(32, 0x43, 13))
        XCTAssertEqual(v["input.client_nonce"], [0xD4, 0xC3, 0xB2, 0xA1])

        let key = ClientKey(devId: v.id("input.dev_id"), kDev: v.secret("input.k_dev"))!
        let hostId = v.id("input.host_id")
        let sessionId: UInt32 = 0x1357_9BDF
        let hello = Self.goldenHello()
        XCTAssertEqual(hello.encode(), v["input.hello_payload"])
        XCTAssertEqual(try HelloPayload.parse(v["input.hello_payload"]), hello)

        let devA = key.devId
        let status = SlotStatus(held: [(devA, .active), (Id16(pattern(16, 0xE0, 1))!, .pending)],
                                tombstones: [Id16(pattern(16, 0xF0, 1))!])
        var accept = Accept()
        accept.sessionId = sessionId
        accept.heartbeatIntervalMs = 1000
        accept.reportIntervalMs = 500
        accept.livenessTimeoutMs = 3000
        accept.streamConfig.streamEpoch = 3
        accept.streamConfig.width = 2560
        accept.streamConfig.height = 1440
        accept.streamConfig.fps = 120
        accept.streamConfig.codec = 1
        accept.streamConfig.bitrateBps = 50_000_000
        accept.hostCaps = 0x003F
        accept.hostName = "TEST-PC"
        accept.slotStatus = status
        XCTAssertEqual(Msg2Payload.accept(accept).encode(), v["input.accept_payload"])
        XCTAssertEqual(try Msg2Payload.parse(v["input.accept_payload"]), .accept(accept))
        XCTAssertEqual(V3.sessionPrologue(devId: devA, hostId: hostId), v["prologue"])

        // Message 1 from the iPad's fixed ephemeral, message 2 from the
        // host's: both sides of the handshake reproduce the Rust bytes.
        let initiator = try Initiator(key: key, hostId: hostId, clientNonce: 0xA1B2_C3D4, payload: hello,
                                      ephemeral: ephemeral(v["input.init_ephemeral"]))
        XCTAssertEqual(hex(initiator.msg1), hex(v["msg1"]))
        let host = try TestResponder(msg1: v["msg1"], kDev: key.kDev, hostId: hostId)
        XCTAssertEqual(host.payload, hello)
        XCTAssertEqual(host.msg1.clientNonce, 0xA1B2_C3D4)
        let (msg2, hostKeys) = host.writeMsg2(.accept(accept), ephemeral: ephemeral(v["input.resp_ephemeral"]))
        XCTAssertEqual(hex(msg2), hex(v["msg2"]))
        let (reply, ipadKeysOrNil) = try initiator.readMsg2(v["msg2"])
        XCTAssertEqual(reply, .accept(accept))
        let ipadKeys = try XCTUnwrap(ipadKeysOrNil)
        XCTAssertEqual(hex(ipadKeys.handshakeHash), hex(v["handshake_hash"]))
        XCTAssertEqual(bytes(ipadKeys.initiatorToResponder), v["key.initiator_to_responder"])
        XCTAssertEqual(bytes(ipadKeys.responderToInitiator), v["key.responder_to_initiator"])
        XCTAssertEqual(try XCTUnwrap(hostKeys).handshakeHash, ipadKeys.handshakeHash)

        // The records that follow, sealed by each side in the same order.
        let i2r = v.secret("key.initiator_to_responder")
        let r2i = v.secret("key.responder_to_initiator")
        let (ipadSend, ipadRecv) = ipadKeys.initiator(sessionId: sessionId)
        let (hostSend, hostRecv) = try XCTUnwrap(hostKeys).responder(sessionId: sessionId)
        let inputBody = Array(0..<30 as Range<UInt8>)
        let mediaBody = (0..<1200).map { UInt8(($0 * 31) % 251) }
        let confirm = SessionMessage.confirm
        let ack = SessionMessage.confirmAck(status)
        let bye = SessionMessage.bye(0)
        let sealed: [(String, Sealer, Opener, UInt8, [UInt8])] = [
            ("record.confirm", ipadSend, hostRecv, confirm.innerType, confirm.encodeBody()),
            ("record.confirm_ack", hostSend, ipadRecv, ack.innerType, ack.encodeBody()),
            ("record.input", ipadSend, hostRecv, V2.inputEventType, inputBody),
            ("record.media_1200", hostSend, ipadRecv, V2.mediaType, mediaBody),
            ("record.bye", ipadSend, hostRecv, bye.innerType, bye.encodeBody()),
        ]
        for (name, sealer, opener, inner, body) in sealed {
            XCTAssertEqual(hex(try sealer.seal(innerType: inner, body: body)), hex(v[name]), name)
            let opened = try opener.open(v[name])
            XCTAssertEqual(opened.innerType, inner, name)
            XCTAssertEqual(opened.body, body, name)
        }
        XCTAssertEqual(try SessionMessage.parse(innerType: ack.innerType, body: ack.encodeBody()), ack)
        // A second open of each is a replay.
        XCTAssertThrowsError(try hostRecv.open(v["record.input"])) { XCTAssertEqual($0 as? V3Error, .replay) }
        XCTAssertThrowsError(try ipadRecv.open(v["record.media_1200"])) { XCTAssertEqual($0 as? V3Error, .replay) }

        // Records at counters whose encoding crosses a byte boundary.
        let counterBody = v["input.counter_body"]
        XCTAssertEqual(counterBody, [UInt8](repeating: 0xC3, count: 32))
        XCTAssertEqual(v["input.counter_inner_type"], [V2.mediaType])
        let boundary = Sealer(key: r2i, sessionId: sessionId)
        for (name, counter) in [("record.counter_255", UInt64(255)), ("record.counter_256", 256),
                                ("record.counter_65535", 65_535), ("record.counter_65536", 65_536),
                                ("record.counter_last", V3.maxRecordsPerDirection - 1)] {
            boundary.skip(to: counter)
            XCTAssertEqual(hex(try boundary.seal(innerType: V2.mediaType, body: counterBody)), hex(v[name]), name)
            XCTAssertEqual(try Opener(key: r2i, sessionId: sessionId).open(v[name]).counter, counter, name)
        }
        XCTAssertThrowsError(try boundary.seal(innerType: V2.mediaType, body: counterBody)) {
            XCTAssertEqual($0 as? V3Error, .exhausted)
        }

        // Must fail authentication, each under fresh state, while the
        // original still works.
        XCTAssertThrowsError(try TestResponder(msg1: v["reject_auth.msg1_tag_flipped"], kDev: key.kDev, hostId: hostId)) {
            XCTAssertEqual($0 as? V3Error, .auth)
        }
        let again = try Initiator(key: key, hostId: hostId, clientNonce: 0xA1B2_C3D4, payload: hello,
                                  ephemeral: ephemeral(v["input.init_ephemeral"]))
        XCTAssertThrowsError(try again.readMsg2(v["reject_auth.msg2_tag_flipped"])) { XCTAssertEqual($0 as? V3Error, .auth) }
        XCTAssertFalse(again.isFinished, "a failed read changes nothing")
        XCTAssertNoThrow(try again.readMsg2(v["msg2"]))
        let freshHost = { Opener(key: i2r, sessionId: sessionId) }
        let freshIpad = { Opener(key: r2i, sessionId: sessionId) }
        for name in ["reject_auth.record_input_body_flipped", "reject_auth.record_input_counter_flipped"] {
            XCTAssertThrowsError(try freshHost().open(v[name]), name) { XCTAssertEqual($0 as? V3Error, .auth) }
        }
        let sessionFlipped = v["reject_auth.record_media_session_flipped"]
        let flippedId = try RecordHeader.parse(sessionFlipped).sessionId
        XCTAssertNotEqual(flippedId, sessionId)
        XCTAssertThrowsError(try Opener(key: r2i, sessionId: flippedId).open(sessionFlipped)) {
            XCTAssertEqual($0 as? V3Error, .auth)
        }
        XCTAssertThrowsError(try freshIpad().open(sessionFlipped)) {
            XCTAssertEqual($0 as? V3Error, .wire(.invalidField("session_id")))
        }
        XCTAssertThrowsError(try freshIpad().open(v["reject_auth.record_input_reflected"])) {
            XCTAssertEqual($0 as? V3Error, .auth)
        }

        // Must not parse.
        XCTAssertThrowsError(try RecordHeader.parse(v["reject_framing.record_length_flipped"])) {
            XCTAssertEqual($0 as? WireError, .lengthMismatch)
        }
        XCTAssertThrowsError(try RecordHeader.parse(v["reject_framing.record_flags_set"])) {
            XCTAssertEqual($0 as? WireError, .invalidField("flags"))
        }
        XCTAssertThrowsError(try Wire.parseMsg1(v["reject_framing.msg1_protocol_2"]))
        XCTAssertThrowsError(try Wire.parseMsg1(v["reject_framing.msg1_trailing_byte"]))
        XCTAssertThrowsError(try Wire.parseMsg2(v["reject_framing.msg2_session_id_set"]))
        XCTAssertThrowsError(try Wire.parseMsg2(v["reject_framing.msg2_truncated"]))
        XCTAssertEqual(Wire.classify(v["reject_framing.msg1_protocol_2"]), .other)

        // Refused before the AEAD: the counter limit.
        XCTAssertThrowsError(try freshHost().open(v["reject_limit.record_counter_2_32"])) {
            XCTAssertEqual($0 as? V3Error, .replay)
        }

        // A Stale answer to the same message 1.
        let staleHost = try TestResponder(msg1: v["msg1"], kDev: key.kDev, hostId: hostId)
        let stale = Msg2Payload.stale(storedTimestampUs: hello.timestampUs + 1)
        XCTAssertEqual(stale.encode(), v["input.stale_payload"])
        let (msg2Stale, staleKeys) = staleHost.writeMsg2(stale, ephemeral: ephemeral(v["input.stale_ephemeral"]))
        XCTAssertNil(staleKeys)
        XCTAssertEqual(hex(msg2Stale), hex(v["msg2_stale"]))
        let staleIpad = try Initiator(key: key, hostId: hostId, clientNonce: 0xA1B2_C3D4, payload: hello,
                                      ephemeral: ephemeral(v["input.init_ephemeral"]))
        let (staleReply, noKeys) = try staleIpad.readMsg2(v["msg2_stale"])
        XCTAssertEqual(staleReply, stale)
        XCTAssertNil(noKeys)

        XCTAssertEqual(Wire.classify(v["msg1"]), .handshake1)
        XCTAssertEqual(Wire.classify(v["msg2"]), .handshake2)
        XCTAssertEqual(Wire.classify(v["record.input"]), .sealed)
        XCTAssertEqual(v.unused, [])
    }

    // MARK: - client_golden.txt

    static func fullStatus() -> SlotStatus {
        SlotStatus(
            held: [(Id16(pattern(16, 0x01, 1))!, .pending), (Id16(pattern(16, 0x11, 1))!, .active),
                   (Id16(pattern(16, 0x21, 1))!, .pending), (Id16(pattern(16, 0x31, 1))!, .pending)],
            tombstones: (0..<8).map { Id16(pattern(16, 0x80 + UInt8($0) * 0x10, 1))! })
    }

    func testClientVectors() throws {
        let s = try Vectors("session_golden.txt")
        let v = try Vectors("client_golden.txt")
        let key = ClientKey(devId: s.id("input.dev_id"), kDev: s.secret("input.k_dev"))!
        let hostId = s.id("input.host_id")
        let hello = Self.goldenHello()
        let freshInitiator = {
            try Initiator(key: key, hostId: hostId, clientNonce: 0xA1B2_C3D4, payload: hello,
                          ephemeral: ephemeral(s["input.init_ephemeral"]))
        }
        let msg1 = try freshInitiator().msg1
        XCTAssertEqual(msg1, s["msg1"])
        let host = { try TestResponder(msg1: msg1, kDev: key.kDev, hostId: hostId) }

        // TooNew and PairAgain answers.
        let tooNew = Msg2Payload.tooNew(limitUs: 1_762_500_000_000_000)
        XCTAssertEqual(tooNew.encode(), v["input.too_new_payload"])
        XCTAssertEqual(v["input.too_new_ephemeral"], pattern(32, 0x87, 19))
        XCTAssertEqual(hex(try host().writeMsg2(tooNew, ephemeral: ephemeral(v["input.too_new_ephemeral"])).0), hex(v["msg2_too_new"]))
        let (tooNewReply, tooNewKeys) = try freshInitiator().readMsg2(v["msg2_too_new"])
        XCTAssertEqual(tooNewReply, tooNew)
        XCTAssertNil(tooNewKeys)

        XCTAssertEqual(Msg2Payload.pairAgain.encode(), v["input.pair_again_payload"])
        XCTAssertEqual(v["input.pair_again_ephemeral"], pattern(32, 0x99, 23))
        XCTAssertEqual(hex(try host().writeMsg2(.pairAgain, ephemeral: ephemeral(v["input.pair_again_ephemeral"])).0), hex(v["msg2_pair_again"]))
        XCTAssertEqual(try freshInitiator().readMsg2(v["msg2_pair_again"]).0, .pairAgain)

        // The largest Accept: a 64-byte name cut before a 2-byte character,
        // a full status.
        let longHostName = String(repeating: "h", count: 63) + "éx"
        XCTAssertEqual(v["input.accept_max_host_name"], Array(longHostName.utf8))
        var accept = Accept()
        accept.sessionId = 0xFEDC_BA98
        accept.heartbeatIntervalMs = 1000
        accept.reportIntervalMs = 500
        accept.livenessTimeoutMs = 3000
        accept.streamConfig.streamEpoch = 0xFFFF_FFFF
        accept.streamConfig.width = 2732
        accept.streamConfig.height = 2048
        accept.streamConfig.fps = 120
        accept.streamConfig.codec = 2
        accept.streamConfig.flags = 0x81
        accept.streamConfig.bitrateBps = 80_000_000
        accept.hostCaps = 0xFFFF
        accept.hostName = longHostName
        accept.slotStatus = Self.fullStatus()
        XCTAssertEqual(Msg2Payload.accept(accept).encode(), v["input.accept_max_payload"])
        XCTAssertEqual(v["input.accept_max_ephemeral"], pattern(32, 0x31, 37))
        let (msg2Max, hostKeys) = try host().writeMsg2(.accept(accept), ephemeral: ephemeral(v["input.accept_max_ephemeral"]))
        XCTAssertEqual(hex(msg2Max), hex(v["msg2_accept_max"]))
        XCTAssertLessThanOrEqual(msg2Max.count, 365)
        let (maxReply, maxKeysOrNil) = try freshInitiator().readMsg2(v["msg2_accept_max"])
        var parsed = accept
        parsed.hostName = String(repeating: "h", count: 63)
        XCTAssertEqual(maxReply, .accept(parsed))
        XCTAssertEqual(try Msg2Payload.parse(v["accept_max.payload_with_trailing_bytes"]), .accept(parsed),
                       "encrypted payloads are append-only")
        let maxKeys = try XCTUnwrap(maxKeysOrNil)
        XCTAssertEqual(hex(maxKeys.handshakeHash), hex(v["accept_max.handshake_hash"]))
        XCTAssertEqual(bytes(maxKeys.initiatorToResponder), v["accept_max.key.initiator_to_responder"])
        XCTAssertEqual(bytes(maxKeys.responderToInitiator), v["accept_max.key.responder_to_initiator"])

        let (ipadSend, ipadRecv) = maxKeys.initiator(sessionId: accept.sessionId)
        let (hostSend, hostRecv) = try XCTUnwrap(hostKeys).responder(sessionId: accept.sessionId)
        let heartbeat = v["input.heartbeat_body"]
        XCTAssertEqual(heartbeat, [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11])
        let refused = SessionMessage.refused(Self.fullStatus())
        for (name, message) in [("accept_max.record.refused", (refused.innerType, refused.encodeBody())),
                                ("accept_max.record.bye", (V2.byeType, [UInt8(2)])),
                                ("accept_max.record.heartbeat", (V2.heartbeatType, heartbeat))] {
            XCTAssertEqual(hex(try hostSend.seal(innerType: message.0, body: message.1)), hex(v[name]), name)
            let opened = try ipadRecv.open(v[name])
            XCTAssertEqual(try SessionMessage.parse(innerType: opened.innerType, body: opened.body),
                           try SessionMessage.parse(innerType: message.0, body: message.1), name)
        }
        XCTAssertEqual(hex(try ipadSend.seal(innerType: SessionMessage.innerConfirm, body: [])), hex(v["accept_max.record.confirm"]))
        XCTAssertEqual(try hostRecv.open(v["accept_max.record.confirm"]).innerType, SessionMessage.innerConfirm)

        // Gap 7: authenticated but unparseable; the attempt is then over.
        for name in v.names(withPrefix: "msg2_unparseable.") where name != "msg2_unparseable.genuine_after" {
            let initiator = try freshInitiator()
            XCTAssertThrowsError(try initiator.readMsg2(v[name]), name) {
                guard case .wire = $0 as? V3Error else { return XCTFail("\(name): \($0)") }
            }
            XCTAssertTrue(initiator.isFinished, name)
            XCTAssertThrowsError(try initiator.readMsg2(v["msg2_unparseable.genuine_after"]), name) {
                XCTAssertEqual($0 as? V3Error, .handshake)
            }
        }
        XCTAssertEqual(v.names(withPrefix: "msg2_unparseable.").count, 4)
        XCTAssertEqual(try freshInitiator().readMsg2(v["msg2_unparseable.genuine_after"]).0, .stale(storedTimestampUs: 1))

        // Encodings.
        let longDeviceName = String(repeating: "n", count: 62) + "日本"
        XCTAssertEqual(v["input.hello_long_name"], Array(longDeviceName.utf8))
        var longHello = hello
        longHello.deviceName = longDeviceName
        XCTAssertEqual(hex(longHello.encode()), hex(v["hello_long_name.payload"]))
        XCTAssertEqual(try HelloPayload.parse(v["hello_long_name.payload"]).deviceName, String(repeating: "n", count: 62))
        var full: [UInt8] = []
        Self.fullStatus().encode(into: &full)
        XCTAssertEqual(full, v["status.full"])
        XCTAssertEqual(full.count, SlotStatus.maxEncodedSize)
        XCTAssertEqual(try SlotStatus.parse(v["status.full"]), Self.fullStatus())
        var empty: [UInt8] = []
        SlotStatus().encode(into: &empty)
        XCTAssertEqual(empty, v["status.empty"])

        // Payloads every parser must reject.
        let rejects = v.names(withPrefix: "reject_parse.")
        XCTAssertEqual(rejects.count, 29)
        for name in rejects {
            let input = v[name]
            switch name.split(separator: ".")[1] {
            case "status": XCTAssertThrowsError(try SlotStatus.parse(input), name)
            case "msg2": XCTAssertThrowsError(try Msg2Payload.parse(input), name)
            case "hello": XCTAssertThrowsError(try HelloPayload.parse(input), name)
            case "session": XCTAssertThrowsError(try SessionMessage.parse(innerType: input[0], body: Array(input.dropFirst())), name)
            default: XCTFail("unknown parser in \(name)")
            }
        }

        try checkReplayScript(v, prefix: "replay_long")
        XCTAssertEqual(v.unused, [])
    }

    // MARK: - replay_vectors.txt

    func testReplayScript() throws {
        let v = try Vectors("replay_vectors.txt")
        try checkReplayScript(v, prefix: "replay")
        XCTAssertEqual(v.unused, [])
    }

    private func checkReplayScript(_ v: Vectors, prefix: String) throws {
        let script = v["\(prefix).counters_u64le"]
        let accepted = v["\(prefix).accepted"]
        XCTAssertEqual(script.count, accepted.count * 8)
        var window = ReplayWindow()
        for (index, expected) in accepted.enumerated() {
            let counter = le64(Array(script[index * 8..<index * 8 + 8]), 0)
            XCTAssertEqual(window.check(counter), expected == 1, "\(prefix) #\(index)")
            XCTAssertEqual(window.checkAndMark(counter) ? 1 : 0, expected, "\(prefix) #\(index) counter \(counter)")
        }
    }
}
