import XCTest
@testable import EternalMonitor

/// Consumes `proto/testdata/v2_vectors.txt` — the exact bytes the Rust
/// `eternal-wire` golden test pins — and proves the Swift codecs agree with
/// them in both directions. The expected field values below are deliberately
/// hardcoded here (not derived from the file) so a wire change must be made
/// consciously on both sides in the same commit.
final class WireProtocolGoldenTests: XCTestCase {
    private static var vectors: [String: Data] = [:]

    override class func setUp() {
        super.setUp()
        guard let url = Bundle(for: WireProtocolGoldenTests.self)
            .url(forResource: "v2_vectors", withExtension: "txt", subdirectory: "testdata"),
            let text = try? String(contentsOf: url, encoding: .utf8)
        else { return }

        for line in text.split(separator: "\n") {
            let line = line.trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty, !line.hasPrefix("#") else { continue }
            let parts = line.split(separator: " ", maxSplits: 1)
            guard parts.count == 2 else { continue }
            vectors[String(parts[0])] = Data(hex: String(parts[1]))
        }
    }

    private func vector(_ name: String) throws -> Data {
        let data = try XCTUnwrap(
            Self.vectors[name],
            "golden vector \(name) missing — is proto/testdata bundled and up to date?"
        )
        return data
    }

    func testGoldenFileIsBundledAndComplete() throws {
        XCTAssertEqual(
            Self.vectors.count, 49,
            "golden vector count drifted — update both test suites together"
        )
    }

    /// The identify request and its answer: nonce A0..AF, port 9876, two
    /// addresses, and the proof of pairing token 01..10 over them, which the
    /// iPad checks as the PC's own.
    func testIdentifyAndIdentityVectors() throws {
        let nonce = Data(0xA0...0xAF)
        let request = try vector("identify")
        let (requestHeader, asked) = try XCTUnwrap(Wire.parseControl(request))
        XCTAssertEqual(asked, .identify(WireIdentify(version: 1, nonce: nonce)))
        XCTAssertEqual(requestHeader.sessionId, 0)
        XCTAssertEqual(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: asked), request)

        let answer = try vector("identity")
        XCTAssertEqual(answer.count, request.count, "never longer than the request")
        let (_, message) = try XCTUnwrap(Wire.parseControl(answer))
        guard case .identity(let identity) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(identity.nonce, nonce)
        XCTAssertEqual(identity.port, 9876)
        XCTAssertEqual(identity.addresses, [Data([192, 0, 2, 10]), Data([198, 51, 100, 10])])
        XCTAssertEqual(identity.proof, MovedPC.proof(token: Data(1...16), nonce: nonce, port: 9876,
                                                     addresses: identity.addresses))
        // More addresses than fit are refused, never read past.
        var five = answer
        five[16 + 1 + 16 + 2] = 5
        XCTAssertNil(Wire.parseControl(five))
        XCTAssertEqual(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: message), answer)
    }

    func testPairingRejectionVectors() throws {
        for (name, status): (String, HelloStatus) in [("hello_ack_unauthorized", .unauthorized), ("hello_ack_rate_limited", .rateLimited)] {
            let data = try vector(name)
            let (header, message) = try XCTUnwrap(Wire.parseControl(data))
            guard case .helloAck(let ack) = message else { return XCTFail("wrong type") }
            XCTAssertEqual(ack.status, status)
            XCTAssertEqual(ack.sessionId, 0)
            XCTAssertEqual(ack.authToken, Data(repeating: 0, count: 16))
            XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        }
    }

    /// 0.3.5: the PC answers while it isn't streaming, a Failed answer with
    /// its reason token.
    func testNotStreamingVectors() throws {
        for (name, status, reason): (String, HelloStatus, String) in [
            ("hello_ack_stopped", .stopped, ""), ("hello_ack_restarting", .restarting, ""),
            ("hello_ack_failed", .failed, "display_changing")
        ] {
            let data = try vector(name)
            let (header, message) = try XCTUnwrap(Wire.parseControl(data))
            guard case .helloAck(let ack) = message else { return XCTFail("wrong type") }
            XCTAssertEqual(ack.status, status)
            XCTAssertTrue(ack.status.pcNotStreaming)
            XCTAssertEqual(ack.reason, reason)
            XCTAssertEqual(ack.sessionId, 0)
            XCTAssertEqual(ack.hostCaps, HelloAck.hostCapNack)
            XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        }
    }

    /// An iPad that asks for standby answers pads its HELLO2 with zeros, as
    /// the PC encodes it, so no answer is larger than the question.
    func testAStandbyHelloIsPaddedToFitAnyAnswer() throws {
        let data = try vector("hello2_standby_padded")
        XCTAssertEqual(data.count, Hello2.standbyLength)
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .hello2(let hello) = message else { return XCTFail("wrong type") }
        XCTAssertNotEqual(hello.featureCaps & Hello2.featureStandbyStatus, 0)
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        // Without the bit, nothing is added.
        var plain = hello
        plain.featureCaps &= ~Hello2.featureStandbyStatus
        XCTAssertLessThan(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: .hello2(plain)).count, data.count)
    }

    /// How 0.3.4 and older treat a status they don't know: the whole ACK is
    /// dropped, as if it were lost. The PC sends 6-8 only to iPads that set
    /// `featureStandbyStatus`.
    func testAnUnknownStatusIsDroppedLikeALostAck() throws {
        var data = try vector("hello_ack_busy")
        data[16] = 9
        XCTAssertNil(Wire.parseControl(data))
        XCTAssertEqual(Hello2.featureStandbyStatus, 1 << 3)
        for status: HelloStatus in [.ok, .busy, .versionUnsupported, .error, .unauthorized, .rateLimited] {
            XCTAssertFalse(status.pcNotStreaming, "\(status)")
        }
    }

    func testNativePenVectorsAndTruncatedTilt() throws {
        for (name, phase, buttons, pressure): (String, UInt8, UInt8, UInt16) in [
            ("input_pen_down", 0, 1, 750), ("input_pen_hover", 1, 0, 0), ("input_pen_cancel", 3, 1, 0)
        ] {
            let data = try vector(name)
            let (header, message) = try XCTUnwrap(Wire.parseControl(data))
            guard case .inputEvent(let event) = message else { return XCTFail("wrong type") }
            XCTAssertEqual(event, WireInputEvent(inputVer: 2, kind: 1, phase: phase, buttons: buttons,
                eventId: 43, xNorm: 65535, yNorm: 16384, pressureX1000: pressure,
                clientTimeUs: 123456789, tiltX: -37, tiltY: 62))
            XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
            for missing in 1...4 {
                var truncated = Data(data.dropLast(missing))
                truncated[6] = UInt8(truncated.count - 16)
                XCTAssertNil(Wire.parseControl(truncated))
            }
        }
    }

    func testKeyboardTextAndHoverVectors() throws {
        for (name, kind, phase, keycode): (String, UInt8, UInt8, UInt16) in [
            ("input_key_down", 4, 0, 0x4f), ("input_text", 5, 0, 0xd83d), ("input_hover", 6, 1, 0)
        ] {
            let data = try vector(name)
            let (header, message) = try XCTUnwrap(Wire.parseControl(data))
            guard case .inputEvent(let event) = message else { return XCTFail("wrong type") }
            XCTAssertEqual(header.sessionId, 0x1234_5678)
            XCTAssertEqual(event, WireInputEvent(kind: kind, phase: phase, buttons: 0, eventId: 42,
                xNorm: 32768, yNorm: 16384, keycode: keycode, modifiers: 2, clientTimeUs: 123456789))
            XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        }
    }

    // MARK: Media

    func testAudioPacketVector() throws { try assertAudioVector("audio_packet", discontinuity: false) }
    func testAudioDiscontinuityVector() throws { try assertAudioVector("audio_discontinuity", discontinuity: true) }

    private func assertAudioVector(_ name: String, discontinuity: Bool) throws {
        let data = try vector(name)
        XCTAssertEqual(Wire.PacketType.audio.rawValue, 0x03)
        XCTAssertEqual(Wire.classify(data), .audio(flags: discontinuity ? 1 : 0))
        let (header, payload) = try XCTUnwrap(AudioHeader.decode(data))
        XCTAssertEqual(header, AudioHeader(sessionId: 0xA1B2_C3D4, streamEpoch: 7,
            audioSeq: 12_345, captureTimestampUs: 0x0000_0123_4567_89AB, discontinuity: discontinuity))
        XCTAssertEqual(Data(data[payload]), Data([0xF8, 0xFF, 0xFE]))
        XCTAssertEqual(header.encode(payload: Data(data[payload])), data)
        // Data slices need not start at index zero.
        var offset = Data([0xAA]); offset.append(data)
        XCTAssertEqual(AudioHeader.decode(offset.dropFirst())?.header, header)
    }

    func testAudioRejectsInvalidFieldsAndPayloadSizes() throws {
        let good = try vector("audio_packet")
        for start in [8, 12] {
            var invalid = good
            invalid.replaceSubrange(start..<start + 4, with: [0, 0, 0, 0])
            XCTAssertNil(AudioHeader.decode(invalid))
        }
        var empty = Data(good.prefix(AudioHeader.size))
        empty[6] = 0; empty[7] = 0
        XCTAssertNil(AudioHeader.decode(empty))
        let header = try XCTUnwrap(AudioHeader.decode(good)).header
        XCTAssertNil(header.encode(payload: Data()))
        XCTAssertNil(header.encode(payload: Data(repeating: 1, count: Wire.maxDatagramSize - AudioHeader.size + 1)))
        let maximum = try XCTUnwrap(header.encode(payload: Data(repeating: 1, count: Wire.maxDatagramSize - AudioHeader.size)))
        XCTAssertEqual(maximum.count, Wire.maxDatagramSize)
        XCTAssertNotNil(AudioHeader.decode(maximum))
        var wrongType = good; wrongType[3] = Wire.PacketType.media.rawValue
        XCTAssertNil(AudioHeader.decode(wrongType))
    }

    func testHello2V030VectorAndLegacyDefaults() throws {
        let data = try vector("hello2_v030")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .hello2(let hello) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(hello.deviceId, 0x0123_4567_89AB_CDEF)
        XCTAssertEqual(hello.preferredFPS, 120)
        XCTAssertEqual(hello.authToken, Data(1...16))
        XCTAssertEqual(hello.pairingCode, 123_456)
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        guard case .hello2(let old) = try XCTUnwrap(Wire.parseControl(vector("hello2"))).message else {
            return XCTFail("wrong legacy type")
        }
        XCTAssertEqual(old.deviceId, 0)
        XCTAssertEqual(old.preferredFPS, 0)
        XCTAssertEqual(old.authToken, Data(repeating: 0, count: 16))
        XCTAssertEqual(old.pairingCode, 0)
    }

    func testMediaKeyframeVector() throws {
        let data = try vector("media_keyframe")
        XCTAssertEqual(Wire.classify(data), .media(flags: 1))
        let (header, payloadRange) = try XCTUnwrap(MediaHeader.decode(data))
        XCTAssertEqual(header.sessionId, 0xA1B2_C3D4)
        XCTAssertEqual(header.streamEpoch, 7)
        XCTAssertEqual(header.frameSeq, 12_345)
        XCTAssertEqual(header.fragIndex, 2)
        XCTAssertEqual(header.fragCount, 9)
        XCTAssertTrue(header.isKeyframe)
        XCTAssertEqual(header.captureTimestampUs, 0x0000_0123_4567_89AB)
        XCTAssertEqual(Data(data[payloadRange]), Data([0xDE, 0xAD, 0xBE, 0xEF]))
        XCTAssertEqual(header.encode(payload: Data(data[payloadRange])), data)
    }

    func testMediaDeltaVector() throws {
        let data = try vector("media_delta")
        let (header, payloadRange) = try XCTUnwrap(MediaHeader.decode(data))
        XCTAssertEqual(header.frameSeq, 12_346)
        XCTAssertEqual(header.fragIndex, 0)
        XCTAssertEqual(header.fragCount, 1)
        XCTAssertFalse(header.isKeyframe)
        XCTAssertEqual(header.captureTimestampUs, 999_999)
        XCTAssertEqual(header.encode(payload: Data(data[payloadRange])), data)
    }

    func testMediaRetransmitVector() throws {
        let data = try vector("media_retransmit")
        XCTAssertEqual(Wire.classify(data), .media(flags: 3))
        let (header, payloadRange) = try XCTUnwrap(MediaHeader.decode(data))
        XCTAssertTrue(header.isKeyframe)
        XCTAssertTrue(header.isRetransmit)
        var original = data
        original[4] &= ~MediaHeader.retransmitFlag
        XCTAssertEqual(original, try vector("media_keyframe"))
        XCTAssertEqual(header.encode(payload: Data(data[payloadRange])), data)
    }

    func testNackVector() throws {
        let data = try vector("nack")
        XCTAssertEqual(Wire.classify(data), .control(.nack))
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(message, .nack(Nack(streamEpoch: 3, frameSeq: 5001, fragCount: 300, missing: [0, 17, 299])))
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
    }

    func testHelloAckPairedVectorAndLegacyDefaults() throws {
        let data = try vector("hello_ack_paired")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .helloAck(let ack) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(ack.authToken, Data(1...16))
        XCTAssertEqual(ack.hostCaps, HelloAck.hostCapNack)
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        let (_, old) = try XCTUnwrap(Wire.parseControl(try vector("hello_ack_ok")))
        guard case .helloAck(let legacy) = old else { return XCTFail("wrong type") }
        XCTAssertEqual(legacy.authToken, Data(repeating: 0, count: 16))
        XCTAssertEqual(legacy.hostCaps, 0)
    }

    /// A host that numbers frames contiguously says so. A new iPad reads the
    /// bit; parsing is the same as 0.3.1's, which stores host caps and tests
    /// only the bits it knows, so an older iPad behaves exactly as before.
    func testHelloAckContiguousFrameSeqVector() throws {
        let data = try vector("hello_ack_contiguous_seq")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .helloAck(let ack) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(HelloAck.hostCapContiguousFrameSeq, 0x0020)
        XCTAssertNotEqual(ack.hostCaps & HelloAck.hostCapContiguousFrameSeq, 0, "a new iPad reads it")
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        // Every bit an older iPad knows reads as it does without the flag.
        let (_, oldMessage) = try XCTUnwrap(Wire.parseControl(try vector("hello_ack_paired")))
        guard case .helloAck(let withoutFlag) = oldMessage else { return XCTFail("wrong type") }
        for bit in [HelloAck.hostCapNack, HelloAck.hostCapAudio, HelloAck.hostCapUSB,
                    HelloAck.hostCapKeyboard, HelloAck.hostCapPen] {
            XCTAssertEqual(ack.hostCaps & bit, withoutFlag.hostCaps & bit)
        }
        XCTAssertEqual(ack.authToken, withoutFlag.authToken)
        XCTAssertEqual(ack.sessionId, withoutFlag.sessionId)
        XCTAssertEqual(ack.streamConfig, withoutFlag.streamConfig)
        // A host without contiguous numbering leaves it clear.
        XCTAssertEqual(withoutFlag.hostCaps & HelloAck.hostCapContiguousFrameSeq, 0)
    }

    func testReceiverReportV030VectorAndLegacyDefaults() throws {
        let data = try vector("receiver_report_v030")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .receiverReport(let report) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(report.fragsRepaired, 1234)
        XCTAssertEqual(report.nacksSent, 567)
        XCTAssertEqual(report.audioPacketsLost, 89)
        XCTAssertEqual(report.audioBufferMs, 60)
        XCTAssertEqual(report.rttMsX10, 28)
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        let (_, old) = try XCTUnwrap(Wire.parseControl(try vector("receiver_report")))
        guard case .receiverReport(let legacy) = old else { return XCTFail("wrong type") }
        XCTAssertEqual(legacy.fragsRepaired, 0)
        XCTAssertEqual(legacy.nacksSent, 0)
        XCTAssertEqual(legacy.audioPacketsLost, 0)
        XCTAssertEqual(legacy.audioBufferMs, 0)
    }

    // MARK: Pointer

    func testPointerCapabilityVectors() throws {
        XCTAssertEqual(Hello2.featureDrawsPointer, 0x0020)
        XCTAssertEqual(HelloAck.hostCapPointer, 0x0040)
        let (_, hello) = try XCTUnwrap(Wire.parseControl(try vector("hello2_draws_pointer")))
        guard case .hello2(let h) = hello else { return XCTFail("wrong type") }
        XCTAssertNotEqual(h.featureCaps & Hello2.featureDrawsPointer, 0)
        let (_, ack) = try XCTUnwrap(Wire.parseControl(try vector("hello_ack_pointer")))
        guard case .helloAck(let a) = ack else { return XCTFail("wrong type") }
        XCTAssertNotEqual(a.hostCaps & HelloAck.hostCapPointer, 0)
        let (_, old) = try XCTUnwrap(Wire.parseControl(try vector("hello_ack_paired")))
        guard case .helloAck(let legacy) = old else { return XCTFail("wrong type") }
        XCTAssertEqual(legacy.hostCaps & HelloAck.hostCapPointer, 0)
    }

    func testPointerShapeVector() throws {
        let data = try vector("pointer_shape")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(header.packetType, .pointerShape)
        guard case .pointerShape(let chunk) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(chunk.shapeId, 0x0A0B_0C0D)
        XCTAssertEqual(chunk.sourceType, 1)
        XCTAssertEqual(chunk.flags, PointerShapeChunk.invertFlag)
        XCTAssertEqual([chunk.width, chunk.height, chunk.hotX, chunk.hotY], [3, 2, 1, 1])
        XCTAssertEqual(chunk.totalLen, 26)
        XCTAssertEqual(chunk.offset, 0)
        XCTAssertEqual(Array(chunk.data.suffix(2)), [0b0010_0000, 0b1000_0000])
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        let image = PointerOverlay.image(chunk, bytes: [UInt8](chunk.data))
        XCTAssertEqual([UInt8](try XCTUnwrap(image.invertMask)), [0, 0, 255, 255, 0, 0])
        XCTAssertEqual([UInt8](image.pixels.prefix(8)), [0, 0, 0, 255, 255, 255, 255, 255])
    }

    func testPointerStateVectors() throws {
        for (name, visible, x, y) in [("pointer_state", true, Int32(-5), Int32(1234)),
                                      ("pointer_hidden", false, 3000, 7)] {
            let data = try vector(name)
            let (header, message) = try XCTUnwrap(Wire.parseControl(data))
            guard case .pointerState(let state) = message else { return XCTFail(name) }
            XCTAssertEqual(state.visible, visible, name)
            XCTAssertEqual(state.x, x, name)
            XCTAssertEqual(state.y, y, name)
            XCTAssertEqual(state.shapeId, 0x0A0B_0C0D, name)
            XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        }
    }

    func testReceiverReportNamesThePointerShape() throws {
        let data = try vector("receiver_report_pointer")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .receiverReport(let report) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(report.pointerShapeId, 0x0A0B_0C0D)
        XCTAssertEqual(report.fragsRepaired, 1234)
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        let (_, old) = try XCTUnwrap(Wire.parseControl(try vector("receiver_report_v030")))
        guard case .receiverReport(let legacy) = old else { return XCTFail("wrong type") }
        XCTAssertEqual(legacy.pointerShapeId, 0)
    }

    func testPointerMessagesRejectWhatRustRejects() throws {
        let good = try vector("pointer_shape")
        // Body offsets: version 16, shape id 20, width 24, total length 32,
        // chunk length 40.
        let breaks: [(String, Range<Int>, [UInt8])] = [
            ("version", 16..<17, [2]), ("zero id", 20..<24, [0, 0, 0, 0]), ("zero width", 24..<26, [0, 0]),
            ("wide", 24..<26, [0x01, 0x02]), ("total length", 32..<36, [99, 0, 0, 0]),
            ("past the end", 36..<40, [1, 0, 0, 0]), ("empty chunk", 40..<42, [0, 0]),
        ]
        for (name, range, bytes) in breaks {
            var bad = good
            bad.replaceSubrange(range, with: bytes)
            XCTAssertNil(Wire.parseControl(bad), name)
        }
        for length in 0..<good.count {
            var truncated = Data(good.prefix(length))
            if truncated.count >= 8 {
                let body = UInt16(max(0, truncated.count - ControlHeader.size))
                truncated[6] = UInt8(body & 0xFF)
                truncated[7] = UInt8(body >> 8)
            }
            XCTAssertNil(Wire.parseControl(truncated), "truncated to \(length)")
        }
        var state = try vector("pointer_state")
        state[16] = 2
        XCTAssertNil(Wire.parseControl(state))
    }

    // MARK: Control

    func testHello2Vector() throws {
        let data = try vector("hello2")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(header.sessionId, 0)
        XCTAssertEqual(header.msgSeq, 1)
        guard case .hello2(let hello) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(hello.protoMin, 2)
        XCTAssertEqual(hello.protoMax, 2)
        XCTAssertEqual(hello.clientNonce, 0xDEAD_BEEF)
        XCTAssertEqual(hello.listenPort, 9876)
        XCTAssertEqual(hello.decoderCaps, Hello2.capDecodeH264 | Hello2.capDecodeHEVC)
        XCTAssertEqual(hello.featureCaps, Hello2.featureWantsInput)
        XCTAssertEqual(hello.screenPxW, 2420)
        XCTAssertEqual(hello.screenPxH, 1668)
        XCTAssertEqual(hello.screenPtW, 1210)
        XCTAssertEqual(hello.screenPtH, 834)
        XCTAssertEqual(hello.refreshHz, 120)
        XCTAssertEqual(hello.deviceName, "Ali's iPad Pro")
        XCTAssertEqual(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: message), data)
    }

    func testHelloAckOkVector() throws {
        let data = try vector("hello_ack_ok")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .helloAck(let ack) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(ack.status, .ok)
        XCTAssertEqual(ack.clientNonce, 0xDEAD_BEEF)
        XCTAssertEqual(ack.sessionId, 0x1234_5678)
        XCTAssertEqual(ack.heartbeatIntervalMs, 1000)
        XCTAssertEqual(ack.reportIntervalMs, 500)
        XCTAssertEqual(ack.livenessTimeoutMs, 3000)
        XCTAssertEqual(ack.streamConfig, Self.sampleConfig)
        XCTAssertEqual(ack.hostName, "ALI-PC")
        XCTAssertEqual(Wire.encodeControl(sessionId: 0, msgSeq: 1, message: message), data)
    }

    func testHelloAckBusyVector() throws {
        let data = try vector("hello_ack_busy")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(header.msgSeq, 2)
        guard case .helloAck(let ack) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(ack.status, .busy)
        XCTAssertEqual(ack.sessionId, 0)
        XCTAssertEqual(ack.streamConfig, StreamConfig())
    }

    /// A refusal without the PC name and host caps, as a HELLO2 too small
    /// for the whole answer gets. Every iPad reads only its status.
    func testHelloAckBusyBareVector() throws {
        let data = try vector("hello_ack_busy_bare")
        XCTAssertEqual(data.count, 49)
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .helloAck(let ack) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(ack.status, .busy)
        XCTAssertEqual(ack.acceptedVersion, 2)
        XCTAssertEqual(ack.clientNonce, 0xDEAD_BEEF)
        XCTAssertEqual(ack.hostName, "")
        XCTAssertEqual(ack.hostCaps, 0)
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
    }

    func testHeartbeatVector() throws {
        let data = try vector("heartbeat")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(header.sessionId, 0x1234_5678)
        XCTAssertEqual(header.msgSeq, 42)
        guard case .heartbeat(let hb) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(hb.hostTimeUs, 987_654_321)
        XCTAssertEqual(hb.streamConfig, Self.sampleConfig)
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: 42, message: message), data)
    }

    /// The PC says it is locked in one byte after the stream config. An
    /// iPad older than the field reads the same heartbeat without it.
    func testPCLockedHeartbeatVector() throws {
        let data = try vector("heartbeat_pc_locked")
        let (header, message) = try XCTUnwrap(Wire.parseControl(data))
        guard case .heartbeat(let hb) = message else { return XCTFail("wrong type") }
        XCTAssertEqual(hb.hostTimeUs, 987_654_321)
        XCTAssertEqual(hb.streamConfig, Self.sampleConfig)
        XCTAssertEqual(hb.notice, Heartbeat.noticePCLocked)
        XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: 42, message: message), data)
        let plain = try vector("heartbeat")
        XCTAssertEqual(data.count, plain.count + 1)
        XCTAssertEqual(data.dropFirst(8).dropLast(), plain.dropFirst(8))
        guard case .heartbeat(let old) = try XCTUnwrap(Wire.parseControl(plain)).message else { return XCTFail("wrong type") }
        XCTAssertEqual(old.notice, Heartbeat.noticeNone)
    }

    /// A PC that takes smaller media datagrams says so, to an iPad that
    /// asked; the request and the frame count are appended to the bodies
    /// earlier versions read, so those read them as before.
    func testMediaSizeVectors() throws {
        XCTAssertEqual(HelloAck.hostCapMediaSize, 0x0080)
        XCTAssertEqual(Hello2.featureMediaSize, 0x0040)
        XCTAssertEqual(KeyframeRequest.minMediaDgram, 576)

        let ackData = try vector("hello_ack_media_size")
        let (ackHeader, ackMessage) = try XCTUnwrap(Wire.parseControl(ackData))
        guard case .helloAck(let ack) = ackMessage else { return XCTFail("wrong type") }
        XCTAssertEqual(ack.hostCaps, HelloAck.hostCapNack | HelloAck.hostCapContiguousFrameSeq | HelloAck.hostCapMediaSize)
        XCTAssertEqual(Wire.encodeControl(sessionId: ackHeader.sessionId, msgSeq: ackHeader.msgSeq, message: ackMessage), ackData)

        let request = try vector("keyframe_request_media_size")
        let (requestHeader, requestMessage) = try XCTUnwrap(Wire.parseControl(request))
        XCTAssertEqual(requestHeader.msgSeq, 51)
        XCTAssertEqual(requestMessage, .keyframeRequest(KeyframeRequest(streamEpoch: 3, lastCompleteSeq: 0, reason: .startup,
                                                                        maxMediaDgram: 1200)))
        XCTAssertEqual(Wire.encodeControl(sessionId: 0x1234_5678, msgSeq: 51, message: requestMessage), request)
        XCTAssertEqual(request.count, try vector("keyframe_request").count + 2)

        let counted = try vector("heartbeat_frames_sent")
        let (heartbeatHeader, heartbeatMessage) = try XCTUnwrap(Wire.parseControl(counted))
        guard case .heartbeat(let hb) = heartbeatMessage else { return XCTFail("wrong type") }
        XCTAssertEqual(hb.framesSent, 1234)
        XCTAssertEqual(hb.notice, Heartbeat.noticeNone)
        XCTAssertEqual(hb.streamConfig, Self.sampleConfig)
        XCTAssertEqual(Wire.encodeControl(sessionId: 0x1234_5678, msgSeq: heartbeatHeader.msgSeq, message: heartbeatMessage),
                       counted)
        let plain = try vector("heartbeat")
        XCTAssertEqual(counted.count, plain.count + 5)
        XCTAssertEqual(counted.dropFirst(16).prefix(plain.count - 16), plain.dropFirst(16))
        guard case .heartbeat(let old) = try XCTUnwrap(Wire.parseControl(plain)).message else { return XCTFail("wrong type") }
        XCTAssertNil(old.framesSent)

        // A notice and the count together: the notice byte, then the count.
        let both = try vector("heartbeat_no_display_frames_sent")
        let (bothHeader, bothMessage) = try XCTUnwrap(Wire.parseControl(both))
        guard case .heartbeat(let noDisplay) = bothMessage else { return XCTFail("wrong type") }
        XCTAssertEqual(noDisplay.notice, Heartbeat.noticeNoDisplay)
        XCTAssertEqual(noDisplay.framesSent, 77)
        XCTAssertEqual(Array(both.dropFirst(plain.count)), [Heartbeat.noticeNoDisplay, 77, 0, 0, 0])
        XCTAssertEqual(Wire.encodeControl(sessionId: 0x1234_5678, msgSeq: bothHeader.msgSeq, message: bothMessage), both)
    }

    func testByeVector() throws {
        let data = try vector("bye_background")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(message, .bye(.appBackground))
    }

    /// The goodbyes a PC sends as it goes to sleep or powers off, only to an
    /// iPad that sets `featurePowerBye`. An app without them drops the whole
    /// message, as 0.3.4 does.
    func testPowerGoodbyeVectors() throws {
        for (name, reason) in [("bye_host_sleeping", ByeReason.hostSleeping),
                               ("bye_host_powering_off", .hostPoweringOff)] {
            let data = try vector(name)
            let (header, message) = try XCTUnwrap(Wire.parseControl(data))
            XCTAssertEqual(message, .bye(reason))
            XCTAssertEqual(Wire.encodeControl(sessionId: header.sessionId, msgSeq: header.msgSeq, message: message), data)
        }
        var unknown = try vector("bye_host_powering_off")
        unknown[unknown.count - 1] = 7
        XCTAssertNil(Wire.parseControl(unknown))
        XCTAssertEqual(Hello2.featurePowerBye, 1 << 7)
    }

    func testKeyframeRequestVector() throws {
        let data = try vector("keyframe_request")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(
            message,
            .keyframeRequest(KeyframeRequest(streamEpoch: 3, lastCompleteSeq: 4111, reason: .gapLoss))
        )
        XCTAssertEqual(Wire.encodeControl(sessionId: 0x1234_5678, msgSeq: 44, message: message), data)
    }

    func testReceiverReportVector() throws {
        let data = try vector("receiver_report")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        var expected = ReceiverReport()
        expected.streamEpoch = 3
        expected.highestSeq = 5000
        expected.framesComplete = 4990
        expected.framesDropped = 10
        expected.fragsReceived = 120_000
        expected.fragsLost = 250
        expected.jitterUs = 2100
        expected.decodeFpsX10 = 599
        expected.assemblerDepth = 1
        expected.decodeDepth = 0
        expected.e2eLatencyMsX10 = 321
        expected.rttMsX10 = 28
        XCTAssertEqual(message, .receiverReport(expected))
        XCTAssertEqual(Wire.encodeControl(sessionId: 0x1234_5678, msgSeq: 45, message: message), data)
    }

    func testPingPongVectors() throws {
        let ping = try vector("ping")
        let (_, pingMessage) = try XCTUnwrap(Wire.parseControl(ping))
        XCTAssertEqual(pingMessage, .ping(WirePing(t1Us: 0x0102_0304_0506_0708)))

        let pong = try vector("pong")
        let (_, pongMessage) = try XCTUnwrap(Wire.parseControl(pong))
        XCTAssertEqual(
            pongMessage,
            .pong(WirePong(
                t1Us: 0x0102_0304_0506_0708,
                t2Us: 0x0102_0304_0506_0710,
                t3Us: 0x0102_0304_0506_0720
            ))
        )
    }

    func testStreamConfigVector() throws {
        let data = try vector("stream_config")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        XCTAssertEqual(message, .streamConfig(Self.sampleConfig))
    }

    func testInputEventVector() throws {
        let data = try vector("input_touch_move")
        let (_, message) = try XCTUnwrap(Wire.parseControl(data))
        var expected = WireInputEvent(
            kind: 0, phase: 1, buttons: 1, eventId: 77,
            xNorm: 32_768, yNorm: 16_384, clientTimeUs: 1_000_001
        )
        expected.pressureX1000 = 500
        expected.scrollDx = -3
        expected.scrollDy = 12
        XCTAssertEqual(message, .inputEvent(expected))
        XCTAssertEqual(Wire.encodeControl(sessionId: 0x1234_5678, msgSeq: 48, message: message), data)
    }

    // MARK: Hardening

    func testNackValidation() throws {
        let valid = try vector("nack")
        for count: UInt8 in [0, 65, 255] {
            var bytes = valid
            bytes[26] = count
            XCTAssertNil(Wire.parseControl(bytes))
        }
        for indices: [UInt16] in [[17, 0, 299], [0, 0, 299], [0, 17, 300]] {
            var bytes = valid
            for (i, value) in indices.enumerated() {
                bytes[27 + i * 2] = UInt8(value & 255)
                bytes[28 + i * 2] = UInt8(value >> 8)
            }
            XCTAssertNil(Wire.parseControl(bytes))
        }
        for length in ControlHeader.size..<valid.count {
            var bytes = Data(valid.prefix(length))
            bytes[6] = UInt8(length - ControlHeader.size)
            bytes[7] = 0
            XCTAssertNil(Wire.parseControl(bytes))
        }
        let maximum = ControlMessage.nack(Nack(streamEpoch: 1, frameSeq: 2, fragCount: 64, missing: Array(0..<64)))
        XCTAssertEqual(Wire.parseControl(Wire.encodeControl(sessionId: 1, msgSeq: 1, message: maximum))?.message, maximum)
    }

    func testEveryVectorTruncationIsRejected() throws {
        for (name, data) in Self.vectors {
            let isMedia = name.hasPrefix("media")
            for length in 0..<data.count {
                let slice = data.prefix(length)
                if isMedia {
                    XCTAssertNil(
                        MediaHeader.decode(slice),
                        "\(name) truncated to \(length) bytes must not parse"
                    )
                } else if name.hasPrefix("audio") {
                    XCTAssertNil(AudioHeader.decode(slice), "\(name) truncated to \(length) bytes must not parse")
                } else {
                    XCTAssertNil(
                        Wire.parseControl(slice),
                        "\(name) truncated to \(length) bytes must not parse"
                    )
                }
            }
        }
    }

    func testClassification() throws {
        var legacy = Data("ETERNALHELLO".utf8)
        legacy.append(contentsOf: [0x94, 0x26])
        XCTAssertEqual(Wire.classify(legacy), .legacyHello)
        XCTAssertEqual(Wire.classify(Data([0, 1, 2])), .unknown)
        XCTAssertEqual(Wire.classify(Data(repeating: 0xFF, count: 64)), .unknown)
        XCTAssertEqual(Wire.classify(try vector("ping")), .control(.ping))
    }

    func testRandomBytesNeverCrashTheParsers() {
        var state: UInt64 = 0x1234_5678_9ABC_DEFF
        func next() -> UInt64 {
            state ^= state << 13
            state ^= state >> 7
            state ^= state << 17
            return state
        }
        for _ in 0..<5000 {
            let length = Int(next() % 80)
            var bytes = [UInt8]()
            bytes.reserveCapacity(length)
            for _ in 0..<length { bytes.append(UInt8(next() & 0xFF)) }
            let data = Data(bytes)
            _ = Wire.classify(data)
            _ = Wire.parseControl(data)
            _ = MediaHeader.decode(data)
            _ = AudioHeader.decode(data)
        }
    }

    private static let sampleConfig = StreamConfig(
        streamEpoch: 3, width: 2560, height: 1440, fps: 60,
        codec: StreamConfig.codecH264, flags: 0, bitrateBps: 15_000_000
    )
}

private extension Data {
    init(hex: String) {
        self.init(capacity: hex.count / 2)
        var iterator = hex.unicodeScalars.makeIterator()
        while let high = iterator.next(), let low = iterator.next() {
            let byte = (UInt8(String(high), radix: 16) ?? 0) << 4
                | (UInt8(String(low), radix: 16) ?? 0)
            append(byte)
        }
    }
}
