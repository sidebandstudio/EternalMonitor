import Foundation

// v3 byte formats, after `proto/src/v3/mod.rs` and `proto/src/v3/wire.rs`:
// the two handshake datagrams, the payloads Noise carries inside them, the
// slot status, and the session messages sealed in records.
//
// Every parser is total: it returns a value or throws a `WireError`, never
// traps, and never reads past its input. Encrypted payloads are append-only
// like v2 control bodies: decoders ignore trailing bytes they don't
// understand. The cleartext handshake framing is exact: no trailing bytes.
// Integers are little-endian throughout, like v2.

/// Protocol constants, shared byte for byte with the Rust reference.
public enum V3 {
    /// The protocol version in HELLO2's `proto_min`/`proto_max`.
    public static let protocolVersion: UInt8 = 3
    /// Noise protocol name. Fixed: any change is a new protocol version.
    public static let noiseParams = "Noise_NNpsk0_25519_ChaChaPoly_SHA256"
    /// First item of the session prologue.
    public static let sessionPrologueLabel = Array("EternalMonitor session v1".utf8)
    /// Packet type of a sealed record. Unknown to v2 receivers.
    public static let sealedType: UInt8 = 0x40
    /// Packet type of handshake message 2.
    public static let handshake2Type: UInt8 = 0x41
    /// Extension tag of the reconnect handshake.
    public static let extSession: UInt16 = 0x0001
    /// Sealed inner types below this are v2 messages, the application's;
    /// from here up they are the protocol's own, and one this side doesn't
    /// know is dropped, never counted as application data.
    public static let firstProtocolInnerType: UInt8 = 0x40

    /// Whether a sealed record's inner type carries an application (v2)
    /// message.
    public static func isApplication(_ innerType: UInt8) -> Bool {
        innerType < firstProtocolInnerType
    }
    /// Sealed records per direction before a fresh handshake is required.
    public static let maxRecordsPerDirection: UInt64 = 1 << 32
    /// The largest message 1 timestamp either side uses or accepts, in µs.
    public static let maxTimestampUs: UInt64 = 1 << 62
    /// A session's maximum age.
    public static let sessionMaxAge: Duration = .seconds(12 * 60 * 60)

    /// HELLO2 shell body: 22 fixed bytes with an empty name, then the
    /// 29-byte tail.
    public static let hello2ShellSize = 51
    /// Extension tag + length.
    public static let extensionHeaderSize = 4
    /// Offset of the extension in message 1.
    public static let msg1ExtensionOffset = V2.controlHeaderSize + hello2ShellSize
    /// Noise `e` plus the AEAD tag on the payload.
    public static let noiseOverhead = 32 + 16
    /// The largest Noise message either side accepts.
    public static let maxNoiseMessage = 1024

    /// `lv_cat`, CPace's length-prefixed concatenation: each item is
    /// preceded by its length as an unsigned LEB128 integer.
    public static func lvCat(_ items: [[UInt8]]) -> [UInt8] {
        var out: [UInt8] = []
        for item in items {
            var length = item.count
            while true {
                let byte = UInt8(length & 0x7F)
                length >>= 7
                if length == 0 {
                    out.append(byte)
                    break
                }
                out.append(byte | 0x80)
            }
            out.append(contentsOf: item)
        }
        return out
    }

    /// The reconnect handshake's Noise prologue:
    /// `lv_cat("EternalMonitor session v1", [3], dev_id, host_id)`.
    public static func sessionPrologue(devId: Id16, hostId: Id16) -> [UInt8] {
        lvCat([sessionPrologueLabel, [protocolVersion], devId.bytes, hostId.bytes])
    }
}

/// The v2 envelope v3 keeps (`proto/src/v2`).
public enum V2 {
    public static let magic: UInt16 = 0x4D45
    public static let version: UInt8 = 2
    public static let prefixSize = 8
    public static let controlHeaderSize = 16
    public static let maxNameLength = 64

    public static let mediaType: UInt8 = 0x01
    public static let hello2Type: UInt8 = 0x10
    public static let heartbeatType: UInt8 = 0x12
    public static let byeType: UInt8 = 0x13
    public static let pingType: UInt8 = 0x16
    public static let receiverReportType: UInt8 = 0x15
    public static let inputEventType: UInt8 = 0x20
}

/// A 16-byte identifier: `dev_id`, `host_id` or `slot_id`. Not secret.
public struct Id16: Hashable, Sendable, CustomStringConvertible {
    public let bytes: [UInt8]

    public init?<Bytes: Collection>(_ bytes: Bytes) where Bytes.Element == UInt8 {
        guard bytes.count == 16 else { return nil }
        self.bytes = Array(bytes)
    }

    public var description: String { bytes.map { String(format: "%02x", $0) }.joined() }
}

/// A byte-level parse failure.
public enum WireError: Error, Equatable, Sendable {
    case truncated
    case badMagic
    case unsupportedVersion(UInt8)
    case unknownType(UInt8)
    case invalidField(String)
    case lengthMismatch
}

/// Why a v3 datagram or payload was rejected. Callers drop these; none
/// ever produces a reply.
public enum V3Error: Error, Equatable, Sendable {
    case wire(WireError)
    /// AEAD or handshake authentication failed.
    case auth
    /// The record counter is outside the replay window or already seen.
    case replay
    /// The sending direction has used up its counters.
    case exhausted
    /// The handshake refused the message for a reason other than
    /// authentication: wrong state (an attempt that's already over).
    case handshake
}

/// Message 1 as it arrives, before any cryptography.
public struct Msg1: Equatable {
    /// From the shell. Unauthenticated; only the version gate uses it.
    public let clientNonce: UInt32
    public let devId: Id16
    public let noise: [UInt8]
}

/// The kind of a v3 datagram, from its first bytes only.
public enum V3Kind: Equatable {
    /// A HELLO2 asking for protocol 3 or above.
    case handshake1
    case handshake2
    case sealed
    /// Anything else, including v2 HELLO2s and v2 control or media traffic.
    case other
}

public enum Wire {
    /// Builds message 1 around a Noise message.
    public static func encodeMsg1(clientNonce: UInt32, listenPort: UInt16, devId: Id16, noise: [UInt8]) -> [UInt8] {
        let extLength = 16 + noise.count
        let bodyLength = V3.hello2ShellSize + V3.extensionHeaderSize + extLength
        var out: [UInt8] = []
        out.reserveCapacity(V2.controlHeaderSize + bodyLength)
        appendPrefix(&out, packetType: V2.hello2Type, bodyLength: bodyLength)
        appendLE(&out, UInt32(0)) // session_id
        appendLE(&out, UInt32(1)) // msg_seq
        out.append(contentsOf: [V3.protocolVersion, V3.protocolVersion])
        appendLE(&out, clientNonce)
        appendLE(&out, listenPort)
        // decoder_caps, feature_caps, four screen fields, refresh_hz,
        // name_len, then the 29-byte tail: 14 + 29 zero bytes.
        out.append(contentsOf: [UInt8](repeating: 0, count: 14 + 29))
        appendLE(&out, V3.extSession)
        appendLE(&out, UInt16(extLength))
        out.append(contentsOf: devId.bytes)
        out.append(contentsOf: noise)
        return out
    }

    /// Parses message 1. Accepts only the exact shell `encodeMsg1` writes.
    public static func parseMsg1(_ datagram: [UInt8]) throws -> Msg1 {
        let body = try controlBody(datagram, packetType: V2.hello2Type)
        var reader = ByteReader(body)
        let shell = try reader.take(V3.hello2ShellSize)
        guard shell[0] == V3.protocolVersion, shell[1] == V3.protocolVersion else {
            throw WireError.invalidField("proto_range")
        }
        let clientNonce = le32(shell, 2)
        guard shell[8...].allSatisfy({ $0 == 0 }) else { throw WireError.invalidField("shell") }
        guard Array(datagram[8..<16]) == [0, 0, 0, 0, 1, 0, 0, 0] else {
            throw WireError.invalidField("header")
        }
        let ext = try extensionPayload(&reader)
        var e = ByteReader(ext)
        let devId = try e.id16()
        let noise = e.rest()
        try checkNoiseLength(noise)
        return Msg1(clientNonce: clientNonce, devId: devId, noise: noise)
    }

    /// Builds message 2 around a Noise message.
    public static func encodeMsg2(noise: [UInt8]) -> [UInt8] {
        var out: [UInt8] = []
        appendPrefix(&out, packetType: V3.handshake2Type, bodyLength: V3.extensionHeaderSize + noise.count)
        out.append(contentsOf: [UInt8](repeating: 0, count: 8))
        appendLE(&out, V3.extSession)
        appendLE(&out, UInt16(noise.count))
        out.append(contentsOf: noise)
        return out
    }

    /// Parses message 2 and returns its Noise message.
    public static func parseMsg2(_ datagram: [UInt8]) throws -> [UInt8] {
        let body = try controlBody(datagram, packetType: V3.handshake2Type)
        guard datagram[8..<16].allSatisfy({ $0 == 0 }) else { throw WireError.invalidField("header") }
        var reader = ByteReader(body)
        let noise = try extensionPayload(&reader)
        try checkNoiseLength(noise)
        return noise
    }

    /// Routes a datagram from its first 18 bytes, without parsing further.
    public static func classify(_ datagram: [UInt8]) -> V3Kind {
        guard datagram.count >= V2.prefixSize,
              le16(datagram, 0) == V2.magic,
              datagram[2] == V2.version
        else { return .other }
        switch datagram[3] {
        case V3.sealedType:
            return .sealed
        case V3.handshake2Type:
            return .handshake2
        case V2.hello2Type where datagram.count > V2.controlHeaderSize + 1
            && datagram[V2.controlHeaderSize + 1] >= V3.protocolVersion:
            return .handshake1
        default:
            return .other
        }
    }

    // MARK: - Shared framing

    static func appendPrefix(_ out: inout [UInt8], packetType: UInt8, bodyLength: Int) {
        appendLE(&out, V2.magic)
        out.append(V2.version)
        out.append(packetType)
        out.append(contentsOf: [0, 0])
        appendLE(&out, UInt16(bodyLength))
    }

    /// Checks magic, version, type, zero flags and the length invariant, and
    /// returns the body after the 16-byte control header.
    static func controlBody(_ datagram: [UInt8], packetType: UInt8) throws -> [UInt8] {
        guard datagram.count >= V2.controlHeaderSize else { throw WireError.truncated }
        guard le16(datagram, 0) == V2.magic else { throw WireError.badMagic }
        guard datagram[2] == V2.version else { throw WireError.unsupportedVersion(datagram[2]) }
        guard datagram[3] == packetType else { throw WireError.unknownType(datagram[3]) }
        guard datagram[4] == 0, datagram[5] == 0 else { throw WireError.invalidField("flags") }
        guard Int(le16(datagram, 6)) == datagram.count - V2.controlHeaderSize else {
            throw WireError.lengthMismatch
        }
        return Array(datagram[V2.controlHeaderSize...])
    }

    /// Reads one extension that runs exactly to the end of the body.
    static func extensionPayload(_ reader: inout ByteReader) throws -> [UInt8] {
        guard try reader.u16() == V3.extSession else { throw WireError.invalidField("extension_tag") }
        let length = Int(try reader.u16())
        guard length == reader.remaining else { throw WireError.lengthMismatch }
        return try reader.take(length)
    }

    static func checkNoiseLength(_ noise: [UInt8]) throws {
        guard noise.count >= V3.noiseOverhead, noise.count <= V3.maxNoiseMessage else {
            throw WireError.invalidField("noise_len")
        }
    }
}

/// The encrypted payload of message 1: a timestamp and the session fields
/// HELLO2 carries in v2.
public struct HelloPayload: Equatable, Sendable {
    /// Strictly increasing per host: `max(clock µs, last + 1)`.
    public var timestampUs: UInt64 = 0
    public var listenPort: UInt16 = 0
    public var decoderCaps: UInt16 = 0
    public var featureCaps: UInt16 = 0
    public var screenPxW: UInt16 = 0
    public var screenPxH: UInt16 = 0
    public var screenPtW: UInt16 = 0
    public var screenPtH: UInt16 = 0
    public var refreshHz: UInt8 = 0
    public var preferredFps: UInt8 = 0
    public var deviceId: UInt64 = 0
    public var deviceName: String = ""

    public init() {}

    public func encode() -> [UInt8] {
        var out: [UInt8] = []
        out.reserveCapacity(40 + deviceName.utf8.count)
        appendLE(&out, timestampUs)
        for value in [listenPort, decoderCaps, featureCaps, screenPxW, screenPxH, screenPtW, screenPtH] {
            appendLE(&out, value)
        }
        out.append(refreshHz)
        out.append(preferredFps)
        appendLE(&out, deviceId)
        appendName(&out, deviceName)
        return out
    }

    public static func parse(_ bytes: [UInt8]) throws -> HelloPayload {
        var r = ByteReader(bytes)
        var p = HelloPayload()
        p.timestampUs = try r.u64()
        p.listenPort = try r.u16()
        p.decoderCaps = try r.u16()
        p.featureCaps = try r.u16()
        p.screenPxW = try r.u16()
        p.screenPxH = try r.u16()
        p.screenPtW = try r.u16()
        p.screenPtH = try r.u16()
        p.refreshHz = try r.u8()
        p.preferredFps = try r.u8()
        p.deviceId = try r.u64()
        p.deviceName = try r.name()
        return p
    }
}

/// State of a credential record the host holds for a slot.
public enum RecordState: UInt8, Sendable {
    case pending = 0
    case active = 1
}

/// What the host holds for one slot, and which keys it has retired.
public struct SlotStatus: Equatable, Sendable {
    /// Records a slot may list: 1 active and 3 pending.
    public static let maxHeld = 4
    /// Tombstones a slot keeps.
    public static let maxTombstones = 8
    /// Largest encoded status: 1 + 4 × 17 + 1 + 8 × 16.
    public static let maxEncodedSize = 198

    public var held: [(devId: Id16, state: RecordState)]
    public var tombstones: [Id16]

    public init(held: [(devId: Id16, state: RecordState)] = [], tombstones: [Id16] = []) {
        self.held = held
        self.tombstones = tombstones
    }

    public static func == (a: SlotStatus, b: SlotStatus) -> Bool {
        a.held.count == b.held.count
            && zip(a.held, b.held).allSatisfy { $0.devId == $1.devId && $0.state == $1.state }
            && a.tombstones == b.tombstones
    }

    /// The state this status lists `devId` in, if it lists it as held.
    public func state(of devId: Id16) -> RecordState? {
        held.first { $0.devId == devId }?.state
    }

    /// Writes the status. Only the tests' host side encodes one; a status
    /// over the caps can't be written.
    func encode(into out: inout [UInt8]) {
        precondition(held.count <= Self.maxHeld && tombstones.count <= Self.maxTombstones)
        out.append(UInt8(held.count))
        for (devId, state) in held {
            out.append(contentsOf: devId.bytes)
            out.append(state.rawValue)
        }
        out.append(UInt8(tombstones.count))
        for devId in tombstones {
            out.append(contentsOf: devId.bytes)
        }
    }

    static func read(_ r: inout ByteReader) throws -> SlotStatus {
        let heldCount = Int(try r.u8())
        guard heldCount <= maxHeld else { throw WireError.invalidField("held_count") }
        var held: [(devId: Id16, state: RecordState)] = []
        for _ in 0..<heldCount {
            let devId = try r.id16()
            guard let state = RecordState(rawValue: try r.u8()) else {
                throw WireError.invalidField("record_state")
            }
            held.append((devId, state))
        }
        guard held.filter({ $0.state == .active }).count <= 1 else {
            throw WireError.invalidField("active_count")
        }
        let tombCount = Int(try r.u8())
        guard tombCount <= maxTombstones else { throw WireError.invalidField("tombstone_count") }
        var tombstones: [Id16] = []
        for _ in 0..<tombCount {
            tombstones.append(try r.id16())
        }
        let all = held.map(\.devId) + tombstones
        guard Set(all).count == all.count else { throw WireError.invalidField("duplicate_dev_id") }
        return SlotStatus(held: held, tombstones: tombstones)
    }

    public static func parse(_ bytes: [UInt8]) throws -> SlotStatus {
        var r = ByteReader(bytes)
        return try read(&r)
    }
}

/// The v2 stream configuration, as carried in the HelloAck.
public struct StreamConfig: Equatable, Sendable {
    public var streamEpoch: UInt32 = 0
    public var width: UInt16 = 0
    public var height: UInt16 = 0
    public var fps: UInt16 = 0
    public var codec: UInt8 = 0
    public var flags: UInt8 = 0
    public var bitrateBps: UInt32 = 0

    public init() {}
}

/// The v3 HelloAck: the v2 fields minus status, version and credential,
/// plus the slot status.
public struct Accept: Equatable, Sendable {
    /// Nonzero. Every record of this session carries it.
    public var sessionId: UInt32 = 0
    public var heartbeatIntervalMs: UInt16 = 0
    public var reportIntervalMs: UInt16 = 0
    public var livenessTimeoutMs: UInt16 = 0
    public var streamConfig = StreamConfig()
    public var hostCaps: UInt16 = 0
    public var hostName: String = ""
    public var slotStatus = SlotStatus()

    public init() {}
}

/// The host's answer inside message 2.
public enum Msg2Payload: Equatable, Sendable {
    case accept(Accept)
    /// The timestamp was not newer than the last committed one. The iPad
    /// jumps past it and tries once more.
    case stale(storedTimestampUs: UInt64)
    /// This key's committed timestamp has no valid successor: pair again.
    case pairAgain
    /// The timestamp was further ahead of the host's clock than it accepts.
    case tooNew(limitUs: UInt64)

    private static let kindAccept: UInt8 = 0
    private static let kindStale: UInt8 = 1
    private static let kindPairAgain: UInt8 = 2
    private static let kindTooNew: UInt8 = 3

    /// Only the tests' host side encodes one.
    func encode() -> [UInt8] {
        var out: [UInt8] = []
        switch self {
        case .accept(let a):
            precondition(a.sessionId != 0)
            out.append(Self.kindAccept)
            appendLE(&out, a.sessionId)
            appendLE(&out, a.heartbeatIntervalMs)
            appendLE(&out, a.reportIntervalMs)
            appendLE(&out, a.livenessTimeoutMs)
            let c = a.streamConfig
            appendLE(&out, c.streamEpoch)
            appendLE(&out, c.width)
            appendLE(&out, c.height)
            appendLE(&out, c.fps)
            out.append(c.codec)
            out.append(c.flags)
            appendLE(&out, c.bitrateBps)
            appendLE(&out, a.hostCaps)
            appendName(&out, a.hostName)
            a.slotStatus.encode(into: &out)
        case .stale(let stored):
            out.append(Self.kindStale)
            appendLE(&out, stored)
        case .pairAgain:
            out.append(Self.kindPairAgain)
        case .tooNew(let limit):
            out.append(Self.kindTooNew)
            appendLE(&out, limit)
        }
        return out
    }

    public static func parse(_ bytes: [UInt8]) throws -> Msg2Payload {
        var r = ByteReader(bytes)
        switch try r.u8() {
        case kindAccept:
            var a = Accept()
            a.sessionId = try r.u32()
            guard a.sessionId != 0 else { throw WireError.invalidField("session_id") }
            a.heartbeatIntervalMs = try r.u16()
            a.reportIntervalMs = try r.u16()
            a.livenessTimeoutMs = try r.u16()
            a.streamConfig.streamEpoch = try r.u32()
            a.streamConfig.width = try r.u16()
            a.streamConfig.height = try r.u16()
            a.streamConfig.fps = try r.u16()
            a.streamConfig.codec = try r.u8()
            a.streamConfig.flags = try r.u8()
            a.streamConfig.bitrateBps = try r.u32()
            a.hostCaps = try r.u16()
            a.hostName = try r.name()
            a.slotStatus = try SlotStatus.read(&r)
            return .accept(a)
        case kindStale:
            return .stale(storedTimestampUs: try r.u64())
        case kindPairAgain:
            return .pairAgain
        case kindTooNew:
            return .tooNew(limitUs: try r.u64())
        default:
            throw WireError.invalidField("msg2_kind")
        }
    }
}

/// The sealed messages the session state machines act on themselves.
/// Inner types below 0x40 are v2 packet types carrying their v2 body.
public enum SessionMessage: Equatable, Sendable {
    /// iPad → host: commit this session. Empty body.
    case confirm
    /// Host → iPad: committed, and durable.
    case confirmAck(SlotStatus)
    /// Host → iPad: this key's record conflicted and is retired.
    case refused(SlotStatus)
    /// Host → iPad: another iPad is streaming; this session wasn't committed
    /// and nothing changed. Empty body.
    case busy
    /// Either way: the session is over. One byte, the v2 `ByeReason`.
    case bye(UInt8)

    public static let innerConfirm: UInt8 = 0x50
    public static let innerConfirmAck: UInt8 = 0x51
    public static let innerRefused: UInt8 = 0x52
    public static let innerBusy: UInt8 = 0x53
    public static let innerBye: UInt8 = V2.byeType

    public var innerType: UInt8 {
        switch self {
        case .confirm: return Self.innerConfirm
        case .confirmAck: return Self.innerConfirmAck
        case .refused: return Self.innerRefused
        case .busy: return Self.innerBusy
        case .bye: return Self.innerBye
        }
    }

    public func encodeBody() -> [UInt8] {
        var out: [UInt8] = []
        switch self {
        case .confirm, .busy: break
        case .confirmAck(let status), .refused(let status): status.encode(into: &out)
        case .bye(let reason): out.append(reason)
        }
        return out
    }

    /// Parses a session message, or returns nil for any other inner type,
    /// which the caller passes on to the application.
    public static func parse(innerType: UInt8, body: [UInt8]) throws -> SessionMessage? {
        var r = ByteReader(body)
        switch innerType {
        case innerConfirm: return .confirm
        case innerConfirmAck: return .confirmAck(try SlotStatus.read(&r))
        case innerRefused: return .refused(try SlotStatus.read(&r))
        case innerBusy: return .busy
        case innerBye: return .bye(try r.u8())
        default: return nil
        }
    }
}

// MARK: - Byte helpers

/// Bounds-checked little-endian reader. Every read returns the value or
/// throws `truncated`; nothing indexes past the input.
struct ByteReader {
    private let data: [UInt8]
    private var at = 0

    init(_ data: [UInt8]) {
        self.data = data
    }

    var remaining: Int { data.count - at }

    mutating func rest() -> [UInt8] {
        let rest = Array(data[at...])
        at = data.count
        return rest
    }

    mutating func take(_ count: Int) throws -> [UInt8] {
        guard count >= 0, count <= remaining else { throw WireError.truncated }
        let slice = Array(data[at..<at + count])
        at += count
        return slice
    }

    mutating func u8() throws -> UInt8 { try take(1)[0] }
    mutating func u16() throws -> UInt16 { le16(try take(2), 0) }
    mutating func u32() throws -> UInt32 { le32(try take(4), 0) }
    mutating func u64() throws -> UInt64 { le64(try take(8), 0) }
    mutating func id16() throws -> Id16 { Id16(try take(16))! }

    /// A length-prefixed UTF-8 name of at most 64 bytes. Ill-formed UTF-8
    /// (overlong forms, surrogates, values above U+10FFFF) is rejected, as
    /// Rust's `String::from_utf8` does: decoding repairs any ill-formed
    /// sequence with U+FFFD, so it round-trips only when well-formed.
    mutating func name() throws -> String {
        let length = Int(try u8())
        guard length <= V2.maxNameLength else { throw WireError.invalidField("name_len") }
        let bytes = try take(length)
        let decoded = String(decoding: bytes, as: UTF8.self)
        guard Array(decoded.utf8) == bytes else { throw WireError.invalidField("name_utf8") }
        return decoded
    }
}

/// Writes a length-prefixed name. Names over 64 bytes are cut at a
/// character (scalar) boundary, as v2 and the Rust reference do.
func appendName(_ out: inout [UInt8], _ name: String) {
    let utf8 = Array(name.utf8)
    var end = min(utf8.count, V2.maxNameLength)
    while end < utf8.count && utf8[end] & 0xC0 == 0x80 {
        end -= 1
    }
    out.append(UInt8(end))
    out.append(contentsOf: utf8[..<end])
}

func appendLE<T: FixedWidthInteger>(_ out: inout [UInt8], _ value: T) {
    withUnsafeBytes(of: value.littleEndian) { out.append(contentsOf: $0) }
}

func le16(_ b: [UInt8], _ at: Int) -> UInt16 {
    UInt16(b[at]) | UInt16(b[at + 1]) << 8
}

func le32(_ b: [UInt8], _ at: Int) -> UInt32 {
    (0..<4).reduce(UInt32(0)) { $0 | UInt32(b[at + $1]) << (8 * $1) }
}

func le64(_ b: [UInt8], _ at: Int) -> UInt64 {
    (0..<8).reduce(UInt64(0)) { $0 | UInt64(b[at + $1]) << (8 * $1) }
}
