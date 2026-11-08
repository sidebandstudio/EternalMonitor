import CryptoKit
import XCTest
@testable import EternalTransportV3

/// Fuzz-like tests. Random and mutated inputs never trap, and every
/// malformed input is rejected:
/// - differentially, against the Rust parsers' verdicts on the same 4,000
///   generated inputs per parser (`parser_differential.txt`);
/// - for authenticated data, every bit flip of a genuine handshake message
///   or record fails, and the genuine one still works afterwards;
/// - through the whole client in each state: mutated datagrams produce no
///   output and change nothing.
final class FuzzTests: XCTestCase {

    // MARK: - Differential against the Rust parsers

    /// The generator of `proto/src/v3/golden_differential.rs`, step for step.
    struct Generator {
        var x: XorShift

        mutating func below(_ n: Int) -> Int { Int(x.next() % UInt64(n)) }

        mutating func mutateOnce(_ out: inout [UInt8]) {
            let len = out.count
            switch x.next() % 5 {
            case 0:
                if len > 0 {
                    let at = below(len)
                    let bit = x.next() % 8
                    out[at] ^= 1 << bit
                }
            case 1:
                out.removeLast(len - below(len + 1))
            case 2:
                if len > 0 {
                    let at = below(len)
                    out[at] = UInt8(truncatingIfNeeded: x.next())
                }
            case 3:
                let n = below(len + 9)
                out = (0..<n).map { _ in UInt8(truncatingIfNeeded: x.next()) }
            default:
                let n = 1 + below(4)
                for _ in 0..<n {
                    out.append(UInt8(truncatingIfNeeded: x.next()))
                }
            }
        }

        mutating func next(_ bases: [[UInt8]]) -> [UInt8] {
            var out = bases[below(bases.count)]
            mutateOnce(&out)
            if x.next() % 3 == 0 {
                mutateOnce(&out)
            }
            return out
        }
    }

    static let kinds: [V3Kind: UInt8] = [.handshake1: 0, .handshake2: 1, .sealed: 2, .other: 3]

    func testParsersAgreeWithTheRustReferenceOnGeneratedInputs() throws {
        let v = try Vectors("parser_differential.txt")
        let s = try Vectors("session_golden.txt")
        let c = try Vectors("client_golden.txt")
        func reencodeStatus(_ status: SlotStatus) -> [UInt8] {
            var out: [UInt8] = []
            status.encode(into: &out)
            return out
        }
        let parsers: [(String, [[UInt8]], ([UInt8]) -> [UInt8]?)] = [
            ("hello", [s["input.hello_payload"], c["hello_long_name.payload"], HelloPayload().encode()],
             { try? HelloPayload.parse($0).encode() }),
            ("status", [c["status.full"], c["status.empty"], c["reject_parse.status.two_active"]],
             { (try? SlotStatus.parse($0)).map(reencodeStatus) }),
            ("msg2_payload", [s["input.accept_payload"], c["input.accept_max_payload"], s["input.stale_payload"],
                              c["input.too_new_payload"], c["input.pair_again_payload"]],
             { try? Msg2Payload.parse($0).encode() }),
            ("session", [[SessionMessage.innerConfirmAck] + c["status.full"], [SessionMessage.innerRefused] + c["status.empty"],
                         [SessionMessage.innerBye, 2], [SessionMessage.innerConfirm], [0x12] + [UInt8](repeating: 0, count: 8)],
             { input in
                 guard let inner = input.first else { return nil }
                 switch Result(catching: { try SessionMessage.parse(innerType: inner, body: Array(input.dropFirst())) }) {
                 case .success(.some(let message)): return [1, message.innerType] + message.encodeBody()
                 case .success(.none): return [2]
                 case .failure: return nil
                 }
             }),
            ("msg1", [s["msg1"]],
             { (try? Wire.parseMsg1($0)).map { m in
                 var out = m.devId.bytes
                 appendLE(&out, m.clientNonce)
                 return out + m.noise
             } }),
            ("msg2", [s["msg2"], c["msg2_accept_max"]], { try? Wire.parseMsg2($0) }),
            ("record_header", [s["record.input"], s["record.confirm"]],
             { (try? RecordHeader.parse($0)).map { h in
                 var out: [UInt8] = []
                 appendLE(&out, h.sessionId)
                 appendLE(&out, h.counter)
                 return out
             } }),
            ("classify", [s["msg1"], s["msg2"], s["record.input"], s["reject_framing.msg1_protocol_2"]],
             { [Self.kinds[Wire.classify($0)]!] }),
        ]
        for (name, bases, parse) in parsers {
            var gen = Generator(x: XorShift(state: le64(v["\(name).seed"], 0)))
            var inputs = SHA256()
            var outputs = SHA256()
            let expected = v["\(name).accepted"]
            var verdicts = [UInt8](repeating: 0, count: expected.count)
            var firstDisagreement: (Int, [UInt8])?
            for index in 0..<4000 {
                let input = gen.next(bases)
                var length = UInt32(input.count).littleEndian
                withUnsafeBytes(of: &length) { inputs.update(bufferPointer: $0) }
                inputs.update(data: input)
                if let output = parse(input) {
                    verdicts[index / 8] |= 1 << (index % 8)
                    length = UInt32(output.count).littleEndian
                    withUnsafeBytes(of: &length) { outputs.update(bufferPointer: $0) }
                    outputs.update(data: output)
                }
                if firstDisagreement == nil, verdicts[index / 8] & (1 << (index % 8)) != expected[index / 8] & (1 << (index % 8)) {
                    firstDisagreement = (index, input)
                }
            }
            XCTAssertEqual(hex(Array(inputs.finalize())), hex(v["\(name).inputs_sha256"]),
                           "\(name): the generator drifted from the Rust one")
            XCTAssertNil(firstDisagreement.map { "case \($0.0): \(hex($0.1))" }, "\(name): first verdict that differs")
            XCTAssertEqual(verdicts, expected, "\(name): verdicts")
            XCTAssertEqual(hex(Array(outputs.finalize())), hex(v["\(name).outputs_sha256"]), "\(name): outputs")
        }
        XCTAssertEqual(v.unused, [])
    }

    // MARK: - Random inputs never trap

    func testRandomBytesNeverTrapAnyParser() {
        var rng = XorShift(state: 0xF00D_FACE_1234_5678)
        for _ in 0..<20_000 {
            let input = rng.bytes(rng.below(300))
            _ = try? Wire.parseMsg1(input)
            _ = try? Wire.parseMsg2(input)
            _ = Wire.classify(input)
            _ = try? RecordHeader.parse(input)
            _ = try? HelloPayload.parse(input)
            _ = try? SlotStatus.parse(input)
            _ = try? Msg2Payload.parse(input)
            if let first = input.first {
                _ = try? SessionMessage.parse(innerType: first, body: Array(input.dropFirst()))
            }
            XCTAssertThrowsError(try input.withUnsafeBytes { try KeyRing.decode($0) })
        }
    }

    func testDamagedStoredRingsAreRejectedOrDecodeToAValidRing() throws {
        var ring = KeyRing(hostId: id16(1), slotId: id16(2))
        ring.add(clientKey(3, 3))
        ring.add(clientKey(4, 4))
        ring.apply(SlotStatus(held: [(id16(3), .active)], tombstones: [id16(9)]))
        let good = bytes(ring.encoded())
        var rng = XorShift(state: 77)
        for _ in 0..<20_000 {
            var bad = good
            switch rng.below(3) {
            case 0: bad[rng.below(bad.count)] ^= UInt8(1 << rng.below(8))
            case 1: bad.removeLast(1 + rng.below(bad.count))
            default: bad[rng.below(bad.count)] = UInt8(truncatingIfNeeded: rng.next())
            }
            if let decoded = try? bad.withUnsafeBytes({ try KeyRing.decode($0) }) {
                // Whatever decodes is a ring this code could have written.
                XCTAssertEqual(bytes(decoded.encoded()), bad)
                XCTAssertLessThanOrEqual(decoded.keys.filter { $0.listed == .active }.count, 1)
            }
        }
    }

    // MARK: - Authenticated data: every flip fails, nothing changes

    func testEveryBitFlipOfMessage2FailsAndTheGenuineOneStillWorks() throws {
        let s = try Vectors("session_golden.txt")
        let key = ClientKey(devId: s.id("input.dev_id"), kDev: s.secret("input.k_dev"))!
        let initiator = try Initiator(key: key, hostId: s.id("input.host_id"), clientNonce: 0xA1B2_C3D4,
                                      payload: GoldenTests.goldenHello(), ephemeral: ephemeral(s["input.init_ephemeral"]))
        let msg2 = s["msg2"]
        for at in 0..<msg2.count {
            for bit in 0..<8 {
                var bad = msg2
                bad[at] ^= 1 << bit
                XCTAssertThrowsError(try initiator.readMsg2(bad), "byte \(at) bit \(bit)")
            }
        }
        for length in 0..<msg2.count {
            XCTAssertThrowsError(try initiator.readMsg2(Array(msg2[..<length])))
        }
        XCTAssertFalse(initiator.isFinished)
        XCTAssertNotNil(try initiator.readMsg2(msg2).1)
    }

    func testEveryBitFlipOfMessage1FailsAtTheHost() throws {
        let s = try Vectors("session_golden.txt")
        let msg1 = s["msg1"]
        for at in 0..<msg1.count {
            var bad = msg1
            bad[at] ^= 0x01
            // The client nonce and listen port in the shell aren't
            // authenticated; the host never reads them as session fields.
            let shellOnly = (18..<24).contains(at)
            let host = Result { try TestResponder(msg1: bad, kDev: s.secret("input.k_dev"), hostId: s.id("input.host_id")) }
            if case .success = host {
                XCTAssertTrue(shellOnly, "byte \(at)")
            }
        }
    }

    // MARK: - The whole client

    /// Mutated copies of every datagram the client sees, fed to a client in
    /// each state: no output, no state change, and the genuine datagram
    /// still works afterwards.
    func testMutatedDatagramsChangeNothingInAnyState() throws {
        let hostId = id16(0xB2)
        let key = clientKey(0xD2, 0x24)
        var rng = XorShift(state: 0xC0FF_EE00_DEAD_BEEF)
        let now: Duration = .seconds(10)

        func mutants(of datagram: [UInt8]) -> [[UInt8]] {
            var out: [[UInt8]] = []
            for _ in 0..<300 {
                var bad = datagram
                switch rng.below(4) {
                case 0: bad[rng.below(bad.count)] ^= UInt8(1 << rng.below(8))
                case 1: bad.removeLast(1 + rng.below(bad.count))
                case 2: bad.append(contentsOf: rng.bytes(1 + rng.below(8)))
                default: bad = rng.bytes(rng.below(bad.count + 8))
                }
                if bad != datagram {
                    out.append(bad)
                }
            }
            return out
        }

        func state(_ client: Client) -> String {
            "\(client.isConnecting) \(client.isConfirming) \(String(describing: client.liveSession)) \(client.abandonedCount) \(client.lastTimestamp)"
        }

        // Connecting: mutated message 2s.
        let connecting = Client(config: ClientConfig(hostId: hostId), hello: HelloPayload(), random: SeededRandom(1))
        let msg1 = connecting.connect(now: now, key: key, clockUs: 1_000).sent[0]
        let host = try TestResponder(msg1: msg1, kDev: key.kDev, hostId: hostId)
        var accept = Accept()
        accept.sessionId = 9
        let (msg2, keys) = host.writeMsg2(.accept(accept))
        let before = state(connecting)
        for bad in mutants(of: msg2) {
            XCTAssertEqual(connecting.handleDatagram(now: now, datagram: bad, clockUs: 1_000), [])
        }
        XCTAssertEqual(state(connecting), before)

        // Confirming: mutated records under the new session.
        XCTAssertEqual(connecting.handleDatagram(now: now, datagram: msg2, clockUs: 1_000).first, .releaseAllInput)
        let (hostSealer, _) = try XCTUnwrap(keys).responder(sessionId: 9)
        let ack = try hostSealer.seal(innerType: SessionMessage.innerConfirmAck, body: [0, 0])
        let confirming = state(connecting)
        for bad in mutants(of: ack) + mutants(of: msg2) {
            XCTAssertEqual(connecting.handleDatagram(now: now, datagram: bad, clockUs: 1_000), [])
        }
        XCTAssertEqual(state(connecting), confirming)

        // Live: mutated records, and the genuine ones still open.
        XCTAssertEqual(connecting.handleDatagram(now: now, datagram: ack, clockUs: 1_000).count, 2)
        let heartbeat = try hostSealer.seal(innerType: V2.heartbeatType, body: [UInt8](repeating: 0, count: 8))
        let live = state(connecting)
        for bad in mutants(of: heartbeat) + mutants(of: ack) {
            XCTAssertEqual(connecting.handleDatagram(now: now, datagram: bad, clockUs: 1_000), [])
        }
        XCTAssertEqual(state(connecting), live)
        XCTAssertEqual(connecting.handleDatagram(now: now, datagram: heartbeat, clockUs: 1_000).count, 1)
        XCTAssertEqual(connecting.handleDatagram(now: now, datagram: ack, clockUs: 1_000), [], "a replayed ack")

        // Random datagrams in every state.
        for _ in 0..<5_000 {
            XCTAssertEqual(connecting.handleDatagram(now: now, datagram: rng.bytes(rng.below(400)), clockUs: 1_000), [])
        }
        XCTAssertEqual(state(connecting), live)
    }
}
