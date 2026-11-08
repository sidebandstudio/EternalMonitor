import CryptoKit
import Foundation
import XCTest
@testable import EternalTransportV3

/// A `name<space>lowercase-hex` vector file from `proto/testdata/v3`, the
/// files the Rust tests pin. Tracks which entries a test read, so a test can
/// assert it reproduced every one.
final class Vectors {
    private let entries: [String: [UInt8]]
    private var used: Set<String> = []
    let file: String

    /// `proto/testdata/v3`, from this file's path in the repository, or
    /// `EM_V3_TESTDATA` if set.
    static var directory: URL {
        if let path = ProcessInfo.processInfo.environment["EM_V3_TESTDATA"] {
            return URL(fileURLWithPath: path)
        }
        return URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // EternalTransportV3Tests
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // EternalTransportV3
            .deletingLastPathComponent() // ios
            .deletingLastPathComponent() // repository
            .appendingPathComponent("proto/testdata/v3")
    }

    init(_ file: String) throws {
        self.file = file
        let text = try String(contentsOf: Self.directory.appendingPathComponent(file), encoding: .utf8)
        var entries: [String: [UInt8]] = [:]
        for line in text.split(separator: "\n", omittingEmptySubsequences: true) where !line.hasPrefix("#") {
            guard let space = line.firstIndex(of: " ") else {
                throw NSError(domain: "Vectors", code: 1, userInfo: [NSLocalizedDescriptionKey: "\(file): no space in \(line)"])
            }
            let name = String(line[..<space])
            let value = try unhex(String(line[line.index(after: space)...]))
            guard entries.updateValue(value, forKey: name) == nil else {
                throw NSError(domain: "Vectors", code: 2, userInfo: [NSLocalizedDescriptionKey: "\(file): duplicate \(name)"])
            }
        }
        self.entries = entries
    }

    subscript(name: String) -> [UInt8] {
        used.insert(name)
        guard let value = entries[name] else {
            XCTFail("\(file): missing \(name)")
            return []
        }
        return value
    }

    func names(withPrefix prefix: String) -> [String] {
        entries.keys.filter { $0.hasPrefix(prefix) }.sorted()
    }

    var count: Int { entries.count }

    /// Entries no test read.
    var unused: [String] { entries.keys.filter { !used.contains($0) }.sorted() }

    func id(_ name: String) -> Id16 { Id16(self[name])! }
    func secret(_ name: String) -> SecretBytes { SecretBytes(copying: self[name]) }
}

func unhex(_ text: String) throws -> [UInt8] {
    let chars = Array(text.utf8)
    guard chars.count % 2 == 0 else { throw NSError(domain: "hex", code: 1) }
    func nibble(_ c: UInt8) throws -> UInt8 {
        switch c {
        case UInt8(ascii: "0")...UInt8(ascii: "9"): return c - UInt8(ascii: "0")
        case UInt8(ascii: "a")...UInt8(ascii: "f"): return c - UInt8(ascii: "a") + 10
        default: throw NSError(domain: "hex", code: 2)
        }
    }
    return try stride(from: 0, to: chars.count, by: 2).map { try nibble(chars[$0]) << 4 | nibble(chars[$0 + 1]) }
}

func hex(_ bytes: [UInt8]) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}

func bytes(_ secret: SecretBytes) -> [UInt8] {
    secret.withUnsafeBytes { Array($0) }
}

/// `pattern(start, step)` from the Rust golden tests.
func pattern(_ count: Int, _ start: UInt8, _ step: UInt8) -> [UInt8] {
    (0..<count).map { start &+ step &* UInt8(truncatingIfNeeded: $0) }
}

func id16(_ byte: UInt8) -> Id16 { Id16([UInt8](repeating: byte, count: 16))! }

func clientKey(_ devByte: UInt8, _ keyByte: UInt8) -> ClientKey {
    ClientKey(devId: id16(devByte), kDev: SecretBytes(copying: [UInt8](repeating: keyByte, count: 32)))!
}

func ephemeral(_ raw: [UInt8]) -> Curve25519.KeyAgreement.PrivateKey {
    try! Curve25519.KeyAgreement.PrivateKey(rawRepresentation: raw)
}

/// Deterministic bytes: the Rust `SeededRandom` (a SHA-256 counter stream
/// over a seed), so a seed gives the same bytes on both sides.
final class SeededRandom: RandomSource {
    private var seed = [UInt8](repeating: 0, count: 32)
    private var counter: UInt64 = 0

    init(_ seed: UInt64) {
        withUnsafeBytes(of: seed.littleEndian) { self.seed.replaceSubrange(0..<8, with: $0) }
    }

    func fill(_ buffer: inout [UInt8]) {
        var at = 0
        while at < buffer.count {
            var hasher = SHA256()
            hasher.update(data: seed)
            withUnsafeBytes(of: counter.littleEndian) { hasher.update(bufferPointer: $0) }
            counter += 1
            let block = Array(hasher.finalize())
            let n = min(32, buffer.count - at)
            buffer.replaceSubrange(at..<at + n, with: block[..<n])
            at += n
        }
    }
}

/// xorshift64, for the fuzz-like tests.
struct XorShift {
    var state: UInt64

    mutating func next() -> UInt64 {
        state ^= state << 13
        state ^= state >> 7
        state ^= state << 17
        return state
    }

    mutating func below(_ n: Int) -> Int { Int(next() % UInt64(n)) }

    mutating func bytes(_ count: Int) -> [UInt8] {
        (0..<count).map { _ in UInt8(truncatingIfNeeded: next()) }
    }
}

/// The host's side of the reconnect handshake, for tests: the Noise
/// responder of NNpsk0 on the module's own `SymmetricState`. The golden
/// tests check its bytes against the Rust host's.
final class TestResponder {
    private var state: SymmetricState
    private let initiatorEphemeral: [UInt8]
    let payload: HelloPayload
    let msg1: Msg1

    init(msg1 datagram: [UInt8], kDev: SecretBytes, hostId: Id16) throws {
        msg1 = try Wire.parseMsg1(datagram)
        var state = SymmetricState(protocolName: V3.noiseParams)
        state.mixHash(V3.sessionPrologue(devId: msg1.devId, hostId: hostId))
        // -> psk, e
        state.mixKeyAndHash(kDev)
        initiatorEphemeral = Array(msg1.noise[..<32])
        state.mixHash(initiatorEphemeral)
        state.mixKey(initiatorEphemeral)
        let plaintext = try state.decryptAndHash(Array(msg1.noise[32...]))
        payload = try HelloPayload.parse(plaintext)
        self.state = state
    }

    /// Writes message 2 and returns it with the keys for an `Accept`.
    func writeMsg2(_ reply: Msg2Payload, ephemeral: Curve25519.KeyAgreement.PrivateKey = .init()) -> ([UInt8], SessionKeys?) {
        writeRaw(reply.encode(), ephemeral: ephemeral, keys: { if case .accept = reply { return true } else { return false } }())
    }

    /// Message 2 around any payload bytes, as only a holder of `K_dev` can
    /// write.
    func writeRaw(_ payload: [UInt8], ephemeral: Curve25519.KeyAgreement.PrivateKey = .init(), keys: Bool = false) -> ([UInt8], SessionKeys?) {
        // <- e, ee
        let ephemeralPublic = Array(ephemeral.publicKey.rawRepresentation)
        state.mixHash(ephemeralPublic)
        state.mixKey(ephemeralPublic)
        let peer = try! Curve25519.KeyAgreement.PublicKey(rawRepresentation: initiatorEphemeral)
        try! ephemeral.sharedSecretFromKeyAgreement(with: peer).withUnsafeBytes { state.mixKey($0) }
        let ciphertext = state.encryptAndHash(payload)
        let datagram = Wire.encodeMsg2(noise: ephemeralPublic + ciphertext)
        return (datagram, keys ? SessionKeys(split: state.split(), handshakeHash: state.handshakeHash) : nil)
    }
}

extension Array where Element == ClientOutput {
    var sent: [[UInt8]] {
        compactMap { if case .send(let datagram) = $0 { return datagram } else { return nil } }
    }
}

let ms = { (n: Int) in Duration.milliseconds(n) }
