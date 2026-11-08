import CryptoKit
import Foundation

// Sealed records, after `proto/src/v3/record.rs`:
//
//     [0..8]    v2 common prefix   type 0x40, flags 0,
//                                  payload_len = datagram_len - 20
//     [8..12]   session_id u32     assigned by the host in message 2
//     [12..20]  counter u64        per direction, from 0, never reused
//     [20..]    ChaCha20-Poly1305  seal(inner_type u8 || body), 16-byte tag
//
// The AAD is the whole 20-byte header; the nonce is the Noise ChaChaPoly
// nonce of the counter. Overhead is 37 bytes. A retransmit is sealed again
// under a fresh counter.

/// The cleartext part of a record, all of it authenticated.
public struct RecordHeader: Equatable, Sendable {
    /// Common prefix, `session_id` and counter.
    public static let size = V2.prefixSize + 4 + 8
    /// Poly1305 tag.
    public static let tagSize = 16
    /// A record is this much larger than the body it carries.
    public static let overhead = size + 1 + tagSize

    public let sessionId: UInt32
    public let counter: UInt64

    /// Reads the header and checks the framing. The AEAD isn't touched.
    public static func parse(_ datagram: [UInt8]) throws -> RecordHeader {
        guard datagram.count >= overhead else { throw WireError.truncated }
        guard le16(datagram, 0) == V2.magic else { throw WireError.badMagic }
        guard datagram[2] == V2.version else { throw WireError.unsupportedVersion(datagram[2]) }
        guard datagram[3] == V3.sealedType else { throw WireError.unknownType(datagram[3]) }
        guard datagram[4] == 0, datagram[5] == 0 else { throw WireError.invalidField("flags") }
        guard Int(le16(datagram, 6)) == datagram.count - size else { throw WireError.lengthMismatch }
        let sessionId = le32(datagram, 8)
        guard sessionId != 0 else { throw WireError.invalidField("session_id") }
        return RecordHeader(sessionId: sessionId, counter: le64(datagram, 12))
    }

    func encode(payloadLength: Int) -> [UInt8] {
        var out: [UInt8] = []
        out.reserveCapacity(Self.size + payloadLength)
        Wire.appendPrefix(&out, packetType: V3.sealedType, bodyLength: payloadLength)
        appendLE(&out, sessionId)
        appendLE(&out, counter)
        return out
    }
}

/// One direction's sending half. Only `SessionKeys` makes one, once per
/// key, so a counter never repeats under a key.
public final class Sealer {
    private let key: SymmetricKey
    public let sessionId: UInt32
    /// Records sealed so far: the next counter.
    public private(set) var sealed: UInt64 = 0

    init(key: SecretBytes, sessionId: UInt32) {
        self.key = key.symmetricKey
        self.sessionId = sessionId
    }

    /// Moves the next counter forward, for tests of the counter limits. It
    /// can't move backwards.
    func skip(to counter: UInt64) {
        sealed = max(sealed, counter)
    }

    /// Seals `inner_type || body` as one record under the next counter.
    ///
    /// Throws `exhausted` once the direction has used 2^32 counters. A body
    /// too large for one datagram (the Rust side asserts on it) throws
    /// before a counter is used.
    public func seal(innerType: UInt8, body: [UInt8]) throws -> [UInt8] {
        guard sealed < V3.maxRecordsPerDirection else { throw V3Error.exhausted }
        let payloadLength = 1 + body.count + RecordHeader.tagSize
        guard payloadLength <= Int(UInt16.max) else { throw V3Error.wire(.invalidField("body_len")) }
        let header = RecordHeader(sessionId: sessionId, counter: sealed)
        sealed += 1
        var record = header.encode(payloadLength: payloadLength)
        var plaintext = [innerType] + body
        defer { wipe(&plaintext) }
        // Can't throw: the key is 32 bytes and the nonce 12.
        let box = try! ChaChaPoly.seal(plaintext, using: key, nonce: noiseNonce(header.counter), authenticating: record)
        record.append(contentsOf: box.ciphertext)
        record.append(contentsOf: box.tag)
        return record
    }
}

/// An authenticated record's content. The description shows the body's
/// length, never its bytes.
public struct Opened: Equatable, CustomStringConvertible {
    public let counter: UInt64
    public let innerType: UInt8
    public let body: [UInt8]

    public var description: String {
        "Opened(counter: \(counter), innerType: \(innerType), bodyLength: \(body.count))"
    }
}

/// One direction's receiving half. `open` is safe to call from several
/// threads; the replay window has its own lock.
public final class Opener {
    private let key: SymmetricKey
    public let sessionId: UInt32
    private var window = ReplayWindow()
    private let lock = NSLock()

    init(key: SecretBytes, sessionId: UInt32) {
        self.key = key.symmetricKey
        self.sessionId = sessionId
    }

    /// The highest counter that has authenticated so far.
    public var highest: UInt64? { lock.withLock { window.top } }

    /// Opens a record. In order, as section 4.4 of the design sets out:
    /// 1. framing, the session id and the counter limit;
    /// 2. a read-only replay check;
    /// 3. the AEAD;
    /// 4. check and mark the counter, under the window's lock.
    ///
    /// A failure at any step leaves the window as it was.
    public func open(_ datagram: [UInt8]) throws -> Opened {
        let header: RecordHeader
        do {
            header = try RecordHeader.parse(datagram)
        } catch let error as WireError {
            throw V3Error.wire(error)
        }
        guard header.sessionId == sessionId else { throw V3Error.wire(.invalidField("session_id")) }
        guard header.counter < V3.maxRecordsPerDirection else { throw V3Error.replay }
        guard lock.withLock({ window.check(header.counter) }) else { throw V3Error.replay }
        let plaintext: [UInt8]
        do {
            let box = try ChaChaPoly.SealedBox(
                nonce: noiseNonce(header.counter),
                ciphertext: datagram[RecordHeader.size..<(datagram.count - RecordHeader.tagSize)],
                tag: datagram[(datagram.count - RecordHeader.tagSize)...])
            plaintext = Array(try ChaChaPoly.open(box, using: key, authenticating: datagram[..<RecordHeader.size]))
        } catch {
            throw V3Error.auth
        }
        guard lock.withLock({ window.checkAndMark(header.counter) }) else { throw V3Error.replay }
        guard let innerType = plaintext.first else { throw V3Error.auth }
        return Opened(counter: header.counter, innerType: innerType, body: Array(plaintext.dropFirst()))
    }
}

/// Per-direction anti-replay window, after RFC 6479 and
/// `proto/src/v3/replay.rs`.
///
/// A ring of 64-bit blocks covers 4,096 counters below the highest accepted
/// so far, plus one spare block so advancing never clears a bit still
/// inside the window. A counter is accepted once, in any order, while
/// `top - counter < 4096`. `check` runs before the AEAD and changes
/// nothing; `checkAndMark` runs after, so only authenticated packets move
/// the window (RFC 4303 §3.4.3).
struct ReplayWindow {
    static let width: UInt64 = 4096
    private static let blockBits: UInt64 = 64
    private static let blockCount = Int(width / blockBits) + 1

    /// Highest counter marked so far, or nil before the first.
    private(set) var top: UInt64?
    private var blocks = [UInt64](repeating: 0, count: blockCount)

    func check(_ counter: UInt64) -> Bool {
        guard let top else { return true }
        if counter > top { return true }
        if top - counter >= Self.width { return false }
        let (block, bit) = Self.position(counter)
        return blocks[block] & bit == 0
    }

    mutating func checkAndMark(_ counter: UInt64) -> Bool {
        guard check(counter) else { return false }
        if let top {
            if counter > top {
                let current = top / Self.blockBits
                let target = counter / Self.blockBits
                let clear = min(target - current, UInt64(Self.blockCount))
                if clear > 0 {
                    for step in 1...clear {
                        blocks[Int((current + step) % UInt64(Self.blockCount))] = 0
                    }
                }
                self.top = counter
            }
        } else {
            blocks = [UInt64](repeating: 0, count: Self.blockCount)
            top = counter
        }
        let (block, bit) = Self.position(counter)
        blocks[block] |= bit
        return true
    }

    private static func position(_ counter: UInt64) -> (Int, UInt64) {
        (Int((counter / blockBits) % UInt64(blockCount)), 1 << (counter % blockBits))
    }
}
