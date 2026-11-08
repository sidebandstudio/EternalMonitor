import Foundation

/// EternalMonitor wire protocol v2 — the Swift mirror of the Rust
/// `eternal-wire` crate's `v2` module. Byte layouts are pinned by
/// `proto/testdata/v2_vectors.txt`, which both implementations' test suites
/// consume; any change here must keep those vectors green on both sides.
///
/// Every v2 datagram starts with an 8-byte common prefix (little-endian):
/// magic "EM" u16 | version u8 = 2 | packetType u8 | flags u8 | reserved u8 |
/// payloadLen u16 (== datagramLength - fixedHeaderLength, enforced strictly).
enum Wire {
    static let magic: UInt16 = 0x4D45  // the bytes "EM" (0x45 0x4D)
    static let version: UInt8 = 2
    static let prefixSize = 8
    static let maxDatagramSize = 1400
    static let legacyHelloMagic = Data("ETERNALHELLO".utf8)

    enum PacketType: UInt8 {
        case media = 0x01
        case mediaFec = 0x02
        case audio = 0x03
        case hello2 = 0x10
        case helloAck = 0x11
        case heartbeat = 0x12
        case bye = 0x13
        case keyframeRequest = 0x14
        case receiverReport = 0x15
        case ping = 0x16
        case pong = 0x17
        case streamConfig = 0x18
        case nack = 0x19
        case pointerShape = 0x1C
        case pointerState = 0x1D
        case inputEvent = 0x20
        case identify = 0x1A
        case identity = 0x1B
        case error = 0x7F
    }

    enum Classification: Equatable {
        case media(flags: UInt8)
        case audio(flags: UInt8)
        case control(PacketType)
        case legacyHello
        case unknown
    }

    /// Routes an inbound datagram without copying.
    static func classify(_ datagram: Data) -> Classification {
        if datagram.count >= legacyHelloMagic.count,
           datagram.prefix(legacyHelloMagic.count) == legacyHelloMagic {
            return .legacyHello
        }
        guard datagram.count >= prefixSize else { return .unknown }
        return datagram.withUnsafeBytes { raw -> Classification in
            guard UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 0, as: UInt16.self)) == magic,
                  raw[2] == version,
                  let type = PacketType(rawValue: raw[3])
            else { return .unknown }
            switch type {
            case .media, .mediaFec:
                return .media(flags: raw[4])
            case .audio:
                return .audio(flags: raw[4])
            default:
                return .control(type)
            }
        }
    }
}

// MARK: - Media

/// One fragment of one encoded access unit. 32-byte header; payload is a raw
/// Annex B chunk. Every fragment repeats the full frame metadata.
struct MediaHeader: Equatable {
    static let size = 32
    static let maxPayload = Wire.maxDatagramSize - size  // 1368
    /// 4 MiB frame cap — larger fragment counts are protocol violations.
    static let maxFragCount: UInt16 = UInt16((4 * 1024 * 1024) / maxPayload)  // 3066
    static let keyframeFlag: UInt8 = 0b0000_0001
    static let retransmitFlag: UInt8 = 0b0000_0010

    var sessionId: UInt32
    var streamEpoch: UInt32
    var frameSeq: UInt32
    var fragIndex: UInt16
    var fragCount: UInt16
    var isKeyframe: Bool
    var isRetransmit: Bool = false
    var captureTimestampUs: UInt64
    var payloadLen: UInt16

    /// Parses and validates a media datagram. Returns the header and the
    /// payload range *within the passed Data* (no copy). Enforces every
    /// protocol invariant so downstream code never re-checks them.
    static func decode(_ datagram: Data) -> (header: MediaHeader, payloadRange: Range<Data.Index>)? {
        guard datagram.count >= size else { return nil }
        let header: MediaHeader? = datagram.withUnsafeBytes { raw in
            guard UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 0, as: UInt16.self)) == Wire.magic,
                  raw[2] == Wire.version,
                  raw[3] == Wire.PacketType.media.rawValue
            else { return nil }
            let payloadLen = UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 6, as: UInt16.self))
            guard Int(payloadLen) == datagram.count - size else { return nil }
            let sessionId = UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: 8, as: UInt32.self))
            let streamEpoch = UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: 12, as: UInt32.self))
            let fragIndex = UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 20, as: UInt16.self))
            let fragCount = UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 22, as: UInt16.self))
            guard sessionId != 0, streamEpoch != 0,
                  fragCount >= 1, fragCount <= maxFragCount, fragIndex < fragCount
            else { return nil }
            return MediaHeader(
                sessionId: sessionId,
                streamEpoch: streamEpoch,
                frameSeq: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: 16, as: UInt32.self)),
                fragIndex: fragIndex,
                fragCount: fragCount,
                isKeyframe: raw[4] & keyframeFlag != 0,
                isRetransmit: raw[4] & retransmitFlag != 0,
                captureTimestampUs: UInt64(littleEndian: raw.loadUnaligned(fromByteOffset: 24, as: UInt64.self)),
                payloadLen: payloadLen
            )
        }
        guard let header else { return nil }
        let start = datagram.index(datagram.startIndex, offsetBy: size)
        return (header, start..<datagram.endIndex)
    }

    /// Serializes a full media datagram (header + payload). Test/tool use.
    func encode(payload: Data) -> Data {
        var out = Data(capacity: Self.size + payload.count)
        out.appendLE(Wire.magic)
        out.append(Wire.version)
        out.append(Wire.PacketType.media.rawValue)
        out.append((isKeyframe ? Self.keyframeFlag : 0) | (isRetransmit ? Self.retransmitFlag : 0))
        out.append(0)
        out.appendLE(UInt16(payload.count))
        out.appendLE(sessionId)
        out.appendLE(streamEpoch)
        out.appendLE(frameSeq)
        out.appendLE(fragIndex)
        out.appendLE(fragCount)
        out.appendLE(captureTimestampUs)
        out.append(payload)
        return out
    }
}

// MARK: - Audio

/// One Opus packet. Audio sequence numbers are independent from video frames.
struct AudioHeader: Equatable {
    static let size = 28
    static let discontinuityFlag: UInt8 = 1
    var sessionId: UInt32
    var streamEpoch: UInt32
    var audioSeq: UInt32
    var captureTimestampUs: UInt64
    var discontinuity: Bool

    static func decode(_ datagram: Data) -> (header: AudioHeader, payloadRange: Range<Data.Index>)? {
        guard datagram.count > size, datagram.count <= Wire.maxDatagramSize else { return nil }
        let header: AudioHeader? = datagram.withUnsafeBytes { raw in
            guard UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 0, as: UInt16.self)) == Wire.magic,
                  raw[2] == Wire.version, raw[3] == Wire.PacketType.audio.rawValue,
                  Int(UInt16(littleEndian: raw.loadUnaligned(fromByteOffset: 6, as: UInt16.self))) == datagram.count - size
            else { return nil }
            let sessionId = UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: 8, as: UInt32.self))
            let streamEpoch = UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: 12, as: UInt32.self))
            guard sessionId != 0, streamEpoch != 0 else { return nil }
            return AudioHeader(
                sessionId: sessionId, streamEpoch: streamEpoch,
                audioSeq: UInt32(littleEndian: raw.loadUnaligned(fromByteOffset: 16, as: UInt32.self)),
                captureTimestampUs: UInt64(littleEndian: raw.loadUnaligned(fromByteOffset: 20, as: UInt64.self)),
                discontinuity: raw[4] & discontinuityFlag != 0
            )
        }
        guard let header else { return nil }
        let start = datagram.index(datagram.startIndex, offsetBy: size)
        return (header, start..<datagram.endIndex)
    }

    func encode(payload: Data) -> Data? {
        guard sessionId != 0, streamEpoch != 0, !payload.isEmpty,
              payload.count <= Wire.maxDatagramSize - Self.size else { return nil }
        var out = Data(capacity: Self.size + payload.count)
        out.appendLE(Wire.magic)
        out.append(Wire.version)
        out.append(Wire.PacketType.audio.rawValue)
        out.append(discontinuity ? Self.discontinuityFlag : 0)
        out.append(0)
        out.appendLE(UInt16(payload.count))
        out.appendLE(sessionId)
        out.appendLE(streamEpoch)
        out.appendLE(audioSeq)
        out.appendLE(captureTimestampUs)
        out.append(payload)
        return out
    }
}

// MARK: - Control messages

struct ControlHeader: Equatable {
    static let size = 16
    var packetType: Wire.PacketType
    var sessionId: UInt32
    var msgSeq: UInt32
}

/// Current stream parameters (16-byte block).
struct StreamConfig: Equatable {
    static let size = 16
    static let codecH264: UInt8 = 0
    static let codecHEVC: UInt8 = 1
    static let flagSoftwareEncoder: UInt8 = 1 << 0

    var streamEpoch: UInt32 = 0
    var width: UInt16 = 0
    var height: UInt16 = 0
    var fps: UInt16 = 0
    var codec: UInt8 = 0
    var flags: UInt8 = 0
    var bitrateBps: UInt32 = 0

    fileprivate func encode(into out: inout Data) {
        out.appendLE(streamEpoch)
        out.appendLE(width)
        out.appendLE(height)
        out.appendLE(fps)
        out.append(codec)
        out.append(flags)
        out.appendLE(bitrateBps)
    }

    fileprivate static func decode(_ r: inout WireReader) -> StreamConfig? {
        guard let streamEpoch: UInt32 = r.readU32(),
              let width: UInt16 = r.readU16(),
              let height: UInt16 = r.readU16(),
              let fps: UInt16 = r.readU16(),
              let codec = r.readU8(),
              let flags = r.readU8(),
              let bitrateBps: UInt32 = r.readU32()
        else { return nil }
        return StreamConfig(
            streamEpoch: streamEpoch, width: width, height: height, fps: fps,
            codec: codec, flags: flags, bitrateBps: bitrateBps
        )
    }
}

struct Hello2: Equatable {
    static let capDecodeH264: UInt16 = 1 << 0
    static let capDecodeHEVC: UInt16 = 1 << 1
    static let capDecodeHEVC10Bit: UInt16 = 1 << 2
    static let featureWantsInput: UInt16 = 1 << 0
    static let featureWantsAudio: UInt16 = 1 << 1
    static let featureSupportsNack: UInt16 = 1 << 2
    /// This app understands the stopped, restarting and failed statuses. A PC
    /// sends them only to an iPad that sets it.
    static let featureStandbyStatus: UInt16 = 1 << 3
    /// A HELLO2 that sets `featureStandbyStatus` is padded with zeros to at
    /// least this many bytes: the longest HELLO_ACK there is. A PC answers
    /// while it isn't streaming only with no more bytes than it was sent.
    static let standbyLength = 164
    /// This app checks for its own updates, so the PC need not tell the user. 0.3.1 and older never set it.
    static let featureChecksUpdates: UInt16 = 1 << 4
    /// This app draws the pointer from POINTER_SHAPE and POINTER_STATE, so
    /// a PC with `HelloAck.hostCapPointer` leaves it out of the video.
    static let featureDrawsPointer: UInt16 = 1 << 5
    /// This iPad may ask for smaller media datagrams in a keyframe request,
    /// and wants heartbeats to count the frames the PC sent. Bit 5 is
    /// `featureDrawsPointer`.
    static let featureMediaSize: UInt16 = 1 << 6
    /// This app understands the goodbyes a PC sends as it goes to sleep or
    /// shuts down. Older apps drop a goodbye they can't read.
    static let featurePowerBye: UInt16 = 1 << 7
    static let maxNameLength = 64

    var protoMin: UInt8 = 2
    var protoMax: UInt8 = 2
    var clientNonce: UInt32
    var listenPort: UInt16
    var decoderCaps: UInt16
    var featureCaps: UInt16
    var screenPxW: UInt16
    var screenPxH: UInt16
    var screenPtW: UInt16
    var screenPtH: UInt16
    var refreshHz: UInt8
    var deviceName: String
    var deviceId: UInt64 = 0
    var preferredFPS: UInt8 = 0
    var authToken = Data(repeating: 0, count: 16)
    var pairingCode: UInt32 = 0
}

enum HelloStatus: UInt8 {
    case ok = 0
    case busy = 1
    case versionUnsupported = 2
    case error = 3
    case unauthorized = 4
    case rateLimited = 5
    /// The PC answered but isn't streaming: someone clicked Stop on it,
    case stopped = 6
    /// it is restarting its stream,
    case restarting = 7
    /// or it stopped after repeated errors (`HelloAck.reason` says why).
    case failed = 8

    /// The PC is there and may stream again: keep asking.
    var pcNotStreaming: Bool { self == .stopped || self == .restarting || self == .failed }
}

struct HelloAck: Equatable {
    static let hostCapNack: UInt16 = 1 << 0
    static let hostCapAudio: UInt16 = 1 << 1
    static let hostCapUSB: UInt16 = 1 << 2
    static let hostCapKeyboard: UInt16 = 1 << 3
    static let hostCapPen: UInt16 = 1 << 4
    /// Media frame_seq counts the frames sent in a stream epoch without gaps,
    /// so a forward gap is a frame lost in transit. Older iPads ignore it.
    /// Every 0.3.4 and later PC sets it in replies that accept a session;
    /// see `OldPC`.
    static let hostCapContiguousFrameSeq: UInt16 = 1 << 5
    /// The PC sends the pointer apart from the video to an iPad that said
    /// it draws it. Older iPads ignore it.
    static let hostCapPointer: UInt16 = 1 << 6
    /// The PC takes `KeyframeRequest.maxMediaDgram` for the rest of the
    /// session, and its heartbeats carry `framesSent`. Only an iPad that
    /// sets `Hello2.featureMediaSize` hears it.
    static let hostCapMediaSize: UInt16 = 1 << 7
    var status: HelloStatus
    var acceptedVersion: UInt8
    var clientNonce: UInt32
    var sessionId: UInt32
    var heartbeatIntervalMs: UInt16
    var reportIntervalMs: UInt16
    var livenessTimeoutMs: UInt16
    var streamConfig: StreamConfig
    var hostName: String
    var authToken = Data(repeating: 0, count: 16)
    var hostCaps: UInt16 = 0
    /// Why the PC isn't streaming, for `.failed`: a token such as
    /// "display_changing". Empty otherwise.
    var reason = ""
    static let maxReasonLength = 32
}

struct Heartbeat: Equatable {
    var hostTimeUs: UInt64
    var streamConfig: StreamConfig
    /// What to say over the picture, for as long as it lasts. PCs older than
    /// the field leave it off: `noticeNone`. Unknown values are ignored.
    var notice: UInt8 = Heartbeat.noticeNone

    static let noticeNone: UInt8 = 0
    /// The PC is locked or shows a permission prompt, so it can't capture.
    static let noticePCLocked: UInt8 = 1
    /// No display is on at the PC (a closed lid, screens off or unplugged).
    static let noticeNoDisplay: UInt8 = 2
    /// Windows won't let EternalMonitor capture the PC's screen from the
    /// graphics card it runs on. The PC's window says how to switch.
    static let noticeUnsupportedGPU: UInt8 = 3
    /// The same in Second screen mode, after the second screen failed to
    /// start: the main screen it fell back to can't be captured either.
    static let noticeSecondScreenUnsupportedGPU: UInt8 = 4
    /// The PC is restarting its stream and keeps this session: the picture
    /// comes back by itself. Sent only to an iPad that sets
    /// `Hello2.featureStandbyStatus`. (3 and 4 are the graphics card notices.)
    static let noticeRestarting: UInt8 = 5
    /// The video frames the PC sent this session, from a PC that offered
    /// `HelloAck.hostCapMediaSize`; nil from others. Appended after
    /// `notice`, which is then always written.
    var framesSent: UInt32?
}

enum ByeReason: UInt8 {
    case userDisconnect = 0
    case appBackground = 1
    case hostShuttingDown = 2
    case error = 3
    case superseded = 4
    /// The PC is going to sleep. Only to an iPad that sets `featurePowerBye`.
    case hostSleeping = 5
    /// The PC is shutting down, restarting or signing out. Only to an iPad
    /// that sets `featurePowerBye`.
    case hostPoweringOff = 6
}

enum KeyframeReason: UInt8 {
    case gapLoss = 0
    case decodeError = 1
    case startup = 2
    case resume = 3
}

struct KeyframeRequest: Equatable {
    var streamEpoch: UInt32
    var lastCompleteSeq: UInt32
    var reason: KeyframeReason
    /// The largest media datagram to send from now on in this session; 0
    /// asks nothing. Appended: only for a PC with `HelloAck.hostCapMediaSize`.
    var maxMediaDgram: UInt16 = 0
    /// The smallest a request may ask for, as the PC's own setting.
    static let minMediaDgram: UInt16 = 576
}

struct Nack: Equatable {
    var streamEpoch: UInt32
    var frameSeq: UInt32
    var fragCount: UInt16
    var missing: [UInt16]

    var isValid: Bool {
        (1...64).contains(missing.count)
            && missing.allSatisfy { $0 < fragCount }
            && zip(missing, missing.dropFirst()).allSatisfy { $0 < $1 }
    }
}

struct ReceiverReport: Equatable {
    var streamEpoch: UInt32 = 0
    var highestSeq: UInt32 = 0
    var framesComplete: UInt32 = 0
    var framesDropped: UInt32 = 0
    var fragsReceived: UInt32 = 0
    var fragsLost: UInt32 = 0
    var jitterUs: UInt32 = 0
    var decodeFpsX10: UInt16 = 0
    var assemblerDepth: UInt8 = 0
    var decodeDepth: UInt8 = 0
    var e2eLatencyMsX10: UInt16 = 0
    var rttMsX10: UInt16 = 0
    var fragsRepaired: UInt32 = 0
    var nacksSent: UInt32 = 0
    var audioPacketsLost: UInt32 = 0
    var audioBufferMs: UInt16 = 0
    /// The last pointer image held complete, 0 for none.
    var pointerShapeId: UInt32 = 0
}

/// One chunk of a pointer image (see the Rust `PointerShapeChunk`). The
/// image is `height` rows of `width` premultiplied BGRA pixels, then, with
/// `invertFlag`, `height` rows of `(width + 7) / 8` mask bytes, most
/// significant bit first: a set bit inverts the picture under that pixel.
struct PointerShapeChunk: Equatable {
    static let version: UInt8 = 1
    static let invertFlag: UInt8 = 1 << 0
    static let maxSide: UInt16 = 512

    var shapeId: UInt32
    var sourceType: UInt8
    var flags: UInt8
    var width: UInt16
    var height: UInt16
    var hotX: UInt16
    var hotY: UInt16
    var totalLen: UInt32
    var offset: UInt32
    var data: Data

    static func imageLength(width: UInt16, height: UInt16, flags: UInt8) -> Int {
        let (w, h) = (Int(width), Int(height))
        return w * h * 4 + (flags & invertFlag != 0 ? (w + 7) / 8 * h : 0)
    }
}

/// Where the PC's pointer is: the image's top-left corner in video pixels.
struct PointerState: Equatable {
    static let visibleFlag: UInt8 = 1 << 0

    var flags: UInt8
    var shapeId: UInt32
    var x: Int32
    var y: Int32
    var hostTimeUs: UInt64

    var visible: Bool { flags & Self.visibleFlag != 0 }
}

struct WirePing: Equatable {
    var t1Us: UInt64
}

struct WirePong: Equatable {
    var t1Us: UInt64
    var t2Us: UInt64
    var t3Us: UInt64
}

/// iPad → PC, session 0: before reconnecting to a PC it found at a new
/// address, the iPad checks that it is the same PC, at that address. The PC
/// answers with a `WireIdentity`. Reserved bytes keep the request as long as
/// the answer.
struct WireIdentify: Equatable {
    static let version: UInt8 = 1
    /// What the proof signs after the nonce.
    static let context = Data("EternalMonitor identify v1".utf8)
    /// Addresses an answer carries at most.
    static let maxAddresses = 4
    /// Both bodies: version, nonce, port, count, address slots and proof.
    static let bodySize = 1 + 16 + 2 + 1 + 4 * maxAddresses + 32
    var version = WireIdentify.version
    var nonce: Data
}

/// PC → iPad, session 0: the port the PC receives the stream on, its home
/// network IPv4 addresses (4 bytes each), and HMAC-SHA256 keyed with the
/// pairing token over `message`.
struct WireIdentity: Equatable {
    var version = WireIdentify.version
    var nonce: Data
    var port: UInt16
    var addresses: [Data]
    var proof: Data

    /// What the proof signs: `nonce ‖ context ‖ port (LE) ‖ count ‖ addresses`.
    static func message(nonce: Data, port: UInt16, addresses: [Data]) -> Data {
        var message = nonce + WireIdentify.context
        message.appendLE(port)
        message.append(UInt8(addresses.count))
        for address in addresses { message.append(address) }
        return message
    }
}

struct WireInputEvent: Equatable {
    var inputVer: UInt8 = 1
    var kind: UInt8
    var phase: UInt8
    var buttons: UInt8
    var eventId: UInt32
    var xNorm: UInt16
    var yNorm: UInt16
    var pressureX1000: UInt16 = 0
    var scrollDx: Int16 = 0
    var scrollDy: Int16 = 0
    var keycode: UInt16 = 0
    var modifiers: UInt8 = 0
    var clientTimeUs: UInt64
    var tiltX: Int16 = 0
    var tiltY: Int16 = 0
}

enum ControlMessage: Equatable {
    case hello2(Hello2)
    case helloAck(HelloAck)
    case heartbeat(Heartbeat)
    case bye(ByeReason)
    case keyframeRequest(KeyframeRequest)
    case nack(Nack)
    case receiverReport(ReceiverReport)
    case ping(WirePing)
    case pong(WirePong)
    case streamConfig(StreamConfig)
    case inputEvent(WireInputEvent)
    case pointerShape(PointerShapeChunk)
    case pointerState(PointerState)
    case identify(WireIdentify)
    case identity(WireIdentity)

    var packetType: Wire.PacketType {
        switch self {
        case .hello2: return .hello2
        case .helloAck: return .helloAck
        case .heartbeat: return .heartbeat
        case .bye: return .bye
        case .keyframeRequest: return .keyframeRequest
        case .nack: return .nack
        case .receiverReport: return .receiverReport
        case .ping: return .ping
        case .pong: return .pong
        case .streamConfig: return .streamConfig
        case .inputEvent: return .inputEvent
        case .pointerShape: return .pointerShape
        case .pointerState: return .pointerState
        case .identify: return .identify
        case .identity: return .identity
        }
    }
}

extension Wire {
    /// Serializes one control datagram (16-byte header + message body).
    static func encodeControl(sessionId: UInt32, msgSeq: UInt32, message: ControlMessage) -> Data {
        var body = Data(capacity: 64)
        switch message {
        case .hello2(let h):
            let name = Data(h.deviceName.utf8).prefixOnCharBoundary(h.deviceName, max: Hello2.maxNameLength)
            body.append(h.protoMin)
            body.append(h.protoMax)
            body.appendLE(h.clientNonce)
            body.appendLE(h.listenPort)
            body.appendLE(h.decoderCaps)
            body.appendLE(h.featureCaps)
            body.appendLE(h.screenPxW)
            body.appendLE(h.screenPxH)
            body.appendLE(h.screenPtW)
            body.appendLE(h.screenPtH)
            body.append(h.refreshHz)
            body.append(UInt8(name.count))
            body.append(name)
            precondition(h.authToken.count == 16)
            if h.deviceId != 0 || h.preferredFPS != 0 || h.authToken.contains(where: { $0 != 0 }) || h.pairingCode != 0 {
                body.appendLE(h.deviceId)
                body.append(h.preferredFPS)
                body.append(h.authToken)
                body.appendLE(h.pairingCode)
            }
            let padded = Hello2.standbyLength - ControlHeader.size
            if h.featureCaps & Hello2.featureStandbyStatus != 0, body.count < padded {
                body.append(Data(count: padded - body.count))
            }
        case .helloAck(let a):
            let name = Data(a.hostName.utf8).prefixOnCharBoundary(a.hostName, max: Hello2.maxNameLength)
            body.append(a.status.rawValue)
            body.append(a.acceptedVersion)
            body.appendLE(a.clientNonce)
            body.appendLE(a.sessionId)
            body.appendLE(a.heartbeatIntervalMs)
            body.appendLE(a.reportIntervalMs)
            body.appendLE(a.livenessTimeoutMs)
            a.streamConfig.encode(into: &body)
            body.append(UInt8(name.count))
            body.append(name)
            precondition(a.authToken.count == 16)
            let reason = Data(a.reason.utf8).prefixOnCharBoundary(a.reason, max: HelloAck.maxReasonLength)
            if a.hostCaps != 0 || a.authToken.contains(where: { $0 != 0 }) || !reason.isEmpty {
                body.append(a.authToken)
                body.appendLE(a.hostCaps)
            }
            if !reason.isEmpty {
                body.append(UInt8(reason.count))
                body.append(reason)
            }
        case .heartbeat(let hb):
            body.appendLE(hb.hostTimeUs)
            hb.streamConfig.encode(into: &body)
            if hb.notice != Heartbeat.noticeNone || hb.framesSent != nil { body.append(hb.notice) }
            if let frames = hb.framesSent { body.appendLE(frames) }
        case .bye(let reason):
            body.append(reason.rawValue)
        case .keyframeRequest(let k):
            body.appendLE(k.streamEpoch)
            body.appendLE(k.lastCompleteSeq)
            body.append(k.reason.rawValue)
            if k.maxMediaDgram != 0 { body.appendLE(k.maxMediaDgram) }
        case .nack(let n):
            precondition(n.isValid)
            body.appendLE(n.streamEpoch)
            body.appendLE(n.frameSeq)
            body.appendLE(n.fragCount)
            body.append(UInt8(n.missing.count))
            for index in n.missing { body.appendLE(index) }
        case .receiverReport(let r):
            body.appendLE(r.streamEpoch)
            body.appendLE(r.highestSeq)
            body.appendLE(r.framesComplete)
            body.appendLE(r.framesDropped)
            body.appendLE(r.fragsReceived)
            body.appendLE(r.fragsLost)
            body.appendLE(r.jitterUs)
            body.appendLE(r.decodeFpsX10)
            body.append(r.assemblerDepth)
            body.append(r.decodeDepth)
            body.appendLE(r.e2eLatencyMsX10)
            body.appendLE(r.rttMsX10)
            if r.fragsRepaired != 0 || r.nacksSent != 0
                || r.audioPacketsLost != 0 || r.audioBufferMs != 0 || r.pointerShapeId != 0 {
                body.appendLE(r.fragsRepaired)
                body.appendLE(r.nacksSent)
                body.appendLE(r.audioPacketsLost)
                body.appendLE(r.audioBufferMs)
            }
            if r.pointerShapeId != 0 {
                body.appendLE(r.pointerShapeId)
            }
        case .ping(let p):
            body.appendLE(p.t1Us)
        case .pong(let p):
            body.appendLE(p.t1Us)
            body.appendLE(p.t2Us)
            body.appendLE(p.t3Us)
        case .streamConfig(let cfg):
            cfg.encode(into: &body)
        case .identify(let i):
            precondition(i.nonce.count == 16)
            body.append(i.version)
            body.append(i.nonce)
            body.append(Data(repeating: 0, count: WireIdentify.bodySize - 17))
        case .identity(let i):
            precondition(i.nonce.count == 16 && i.proof.count == 32 && i.addresses.count <= WireIdentify.maxAddresses
                         && i.addresses.allSatisfy { $0.count == 4 })
            body.append(i.version)
            body.append(i.nonce)
            body.appendLE(i.port)
            body.append(UInt8(i.addresses.count))
            for slot in 0..<WireIdentify.maxAddresses {
                body.append(slot < i.addresses.count ? i.addresses[slot] : Data(count: 4))
            }
            body.append(i.proof)
        case .inputEvent(let e):
            body.append(e.inputVer)
            body.append(e.kind)
            body.append(e.phase)
            body.append(e.buttons)
            body.appendLE(e.eventId)
            body.appendLE(e.xNorm)
            body.appendLE(e.yNorm)
            body.appendLE(e.pressureX1000)
            body.appendLE(UInt16(bitPattern: e.scrollDx))
            body.appendLE(UInt16(bitPattern: e.scrollDy))
            body.appendLE(e.keycode)
            body.append(e.modifiers)
            body.append(0)  // reserved
            body.appendLE(e.clientTimeUs)
            if e.inputVer == 2 {
                body.appendLE(UInt16(bitPattern: e.tiltX))
                body.appendLE(UInt16(bitPattern: e.tiltY))
            }
        case .pointerShape(let c):
            body.append(PointerShapeChunk.version)
            body.append(c.sourceType)
            body.append(c.flags)
            body.append(0)  // reserved
            body.appendLE(c.shapeId)
            body.appendLE(c.width)
            body.appendLE(c.height)
            body.appendLE(c.hotX)
            body.appendLE(c.hotY)
            body.appendLE(c.totalLen)
            body.appendLE(c.offset)
            body.appendLE(UInt16(c.data.count))
            body.append(c.data)
        case .pointerState(let p):
            body.append(PointerShapeChunk.version)
            body.append(p.flags)
            body.appendLE(UInt16(0))  // reserved
            body.appendLE(p.shapeId)
            body.appendLE(UInt32(bitPattern: p.x))
            body.appendLE(UInt32(bitPattern: p.y))
            body.appendLE(p.hostTimeUs)
        }

        var out = Data(capacity: ControlHeader.size + body.count)
        out.appendLE(magic)
        out.append(version)
        out.append(message.packetType.rawValue)
        out.append(0)  // flags
        out.append(0)  // reserved
        out.appendLE(UInt16(body.count))
        out.appendLE(sessionId)
        out.appendLE(msgSeq)
        out.append(body)
        return out
    }

    /// Parses one control datagram. Enforces the strict length invariant and
    /// every field invariant; ignores trailing body bytes (append-only
    /// evolution). Returns nil on any violation — parsing never traps.
    static func parseControl(_ datagram: Data) -> (header: ControlHeader, message: ControlMessage)? {
        guard datagram.count >= ControlHeader.size else { return nil }
        let bytes = [UInt8](datagram)
        guard UInt16(bytes[0]) | (UInt16(bytes[1]) << 8) == magic,
              bytes[2] == version,
              let type = PacketType(rawValue: bytes[3])
        else { return nil }
        let payloadLen = UInt16(bytes[6]) | (UInt16(bytes[7]) << 8)
        guard Int(payloadLen) == bytes.count - ControlHeader.size else { return nil }

        let header = ControlHeader(
            packetType: type,
            sessionId: UInt32(littleEndianBytes: bytes, at: 8),
            msgSeq: UInt32(littleEndianBytes: bytes, at: 12)
        )
        var r = WireReader(bytes: bytes, at: ControlHeader.size)

        let message: ControlMessage?
        switch type {
        case .hello2:
            message = parseHello2(&r)
        case .helloAck:
            message = parseHelloAck(&r)
        case .heartbeat:
            guard let hostTimeUs = r.readU64(),
                  let cfg = StreamConfig.decode(&r) else { return nil }
            message = .heartbeat(Heartbeat(hostTimeUs: hostTimeUs, streamConfig: cfg,
                                           notice: r.readU8() ?? Heartbeat.noticeNone,
                                           framesSent: r.remaining >= 4 ? r.readU32() : nil))
        case .bye:
            guard let raw = r.readU8(), let reason = ByeReason(rawValue: raw) else { return nil }
            message = .bye(reason)
        case .keyframeRequest:
            guard let epoch: UInt32 = r.readU32(),
                  let last: UInt32 = r.readU32(),
                  let raw = r.readU8(),
                  let reason = KeyframeReason(rawValue: raw) else { return nil }
            message = .keyframeRequest(KeyframeRequest(streamEpoch: epoch, lastCompleteSeq: last, reason: reason,
                                                       maxMediaDgram: r.remaining >= 2 ? r.readU16() ?? 0 : 0))
        case .receiverReport:
            message = parseReceiverReport(&r)
        case .nack:
            guard let epoch = r.readU32(), let seq = r.readU32(),
                  let count = r.readU16(), let length = r.readU8(), (1...64).contains(length)
            else { return nil }
            var missing: [UInt16] = []
            for _ in 0..<length {
                guard let index = r.readU16() else { return nil }
                missing.append(index)
            }
            let nack = Nack(streamEpoch: epoch, frameSeq: seq, fragCount: count, missing: missing)
            guard nack.isValid else { return nil }
            message = .nack(nack)
        case .ping:
            guard let t1 = r.readU64() else { return nil }
            message = .ping(WirePing(t1Us: t1))
        case .pong:
            guard let t1 = r.readU64(), let t2 = r.readU64(), let t3 = r.readU64() else { return nil }
            message = .pong(WirePong(t1Us: t1, t2Us: t2, t3Us: t3))
        case .streamConfig:
            guard let cfg = StreamConfig.decode(&r) else { return nil }
            message = .streamConfig(cfg)
        case .inputEvent:
            message = parseInputEvent(&r)
        case .pointerShape:
            message = parsePointerShape(&r)
        case .pointerState:
            guard r.readU8() == PointerShapeChunk.version,
                  let flags = r.readU8(), let _ = r.readU16(),
                  let shapeId = r.readU32(), let x = r.readU32(), let y = r.readU32(),
                  let time = r.readU64() else { return nil }
            message = .pointerState(PointerState(
                flags: flags, shapeId: shapeId, x: Int32(bitPattern: x), y: Int32(bitPattern: y),
                hostTimeUs: time))
        case .identify:
            guard let version = r.readU8(), let nonce = r.take(16), r.take(WireIdentify.bodySize - 17) != nil else { return nil }
            message = .identify(WireIdentify(version: version, nonce: Data(nonce)))
        case .identity:
            guard let version = r.readU8(), let nonce = r.take(16), let port = r.readU16(), let count = r.readU8(),
                  Int(count) <= WireIdentify.maxAddresses, let slots = r.take(4 * WireIdentify.maxAddresses),
                  let proof = r.take(32) else { return nil }
            let addresses = (0..<Int(count)).map { Data(slots.dropFirst($0 * 4).prefix(4)) }
            message = .identity(WireIdentity(version: version, nonce: Data(nonce), port: port, addresses: addresses,
                                             proof: Data(proof)))
        case .media, .mediaFec, .audio, .error:
            return nil
        }
        guard let message else { return nil }
        return (header, message)
    }

    private static func parseHello2(_ r: inout WireReader) -> ControlMessage? {
        guard let protoMin = r.readU8(), let protoMax = r.readU8(), protoMin <= protoMax,
              let clientNonce: UInt32 = r.readU32(),
              let listenPort: UInt16 = r.readU16(),
              let decoderCaps: UInt16 = r.readU16(),
              let featureCaps: UInt16 = r.readU16(),
              let screenPxW: UInt16 = r.readU16(),
              let screenPxH: UInt16 = r.readU16(),
              let screenPtW: UInt16 = r.readU16(),
              let screenPtH: UInt16 = r.readU16(),
              let refreshHz = r.readU8(),
              let deviceName = r.readName()
        else { return nil }
        var hello = Hello2(
            protoMin: protoMin, protoMax: protoMax, clientNonce: clientNonce,
            listenPort: listenPort, decoderCaps: decoderCaps, featureCaps: featureCaps,
            screenPxW: screenPxW, screenPxH: screenPxH,
            screenPtW: screenPtW, screenPtH: screenPtH,
            refreshHz: refreshHz, deviceName: deviceName)
        if r.remaining >= 29 {
            guard let deviceId = r.readU64(), let fps = r.readU8(),
                  let token = r.take(16), let code = r.readU32() else { return nil }
            hello.deviceId = deviceId
            hello.preferredFPS = fps
            hello.authToken = Data(token)
            hello.pairingCode = code
        }
        return .hello2(hello)
    }

    private static func parseHelloAck(_ r: inout WireReader) -> ControlMessage? {
        guard let statusRaw = r.readU8(), let status = HelloStatus(rawValue: statusRaw),
              let acceptedVersion = r.readU8(),
              let clientNonce: UInt32 = r.readU32(),
              let sessionId: UInt32 = r.readU32(),
              let heartbeatIntervalMs: UInt16 = r.readU16(),
              let reportIntervalMs: UInt16 = r.readU16(),
              let livenessTimeoutMs: UInt16 = r.readU16(),
              let streamConfig = StreamConfig.decode(&r),
              let hostName = r.readName()
        else { return nil }
        if status == .ok && sessionId == 0 { return nil }
        var ack = HelloAck(
            status: status, acceptedVersion: acceptedVersion, clientNonce: clientNonce,
            sessionId: sessionId, heartbeatIntervalMs: heartbeatIntervalMs,
            reportIntervalMs: reportIntervalMs, livenessTimeoutMs: livenessTimeoutMs,
            streamConfig: streamConfig, hostName: hostName)
        if r.remaining >= 18 {
            guard let token = r.take(16), let caps = r.readU16() else { return nil }
            ack.authToken = Data(token)
            ack.hostCaps = caps
        }
        if r.remaining > 0 {
            guard let length = r.readU8(), Int(length) <= HelloAck.maxReasonLength, let bytes = r.take(Int(length)),
                  let reason = String(bytes: bytes, encoding: .utf8) else { return nil }
            ack.reason = reason
        }
        return .helloAck(ack)
    }

    private static func parseReceiverReport(_ r: inout WireReader) -> ControlMessage? {
        guard let streamEpoch: UInt32 = r.readU32(),
              let highestSeq: UInt32 = r.readU32(),
              let framesComplete: UInt32 = r.readU32(),
              let framesDropped: UInt32 = r.readU32(),
              let fragsReceived: UInt32 = r.readU32(),
              let fragsLost: UInt32 = r.readU32(),
              let jitterUs: UInt32 = r.readU32(),
              let decodeFpsX10: UInt16 = r.readU16(),
              let assemblerDepth = r.readU8(),
              let decodeDepth = r.readU8(),
              let e2eLatencyMsX10: UInt16 = r.readU16(),
              let rttMsX10: UInt16 = r.readU16()
        else { return nil }
        var report = ReceiverReport(
            streamEpoch: streamEpoch, highestSeq: highestSeq,
            framesComplete: framesComplete, framesDropped: framesDropped,
            fragsReceived: fragsReceived, fragsLost: fragsLost, jitterUs: jitterUs,
            decodeFpsX10: decodeFpsX10, assemblerDepth: assemblerDepth,
            decodeDepth: decodeDepth, e2eLatencyMsX10: e2eLatencyMsX10, rttMsX10: rttMsX10)
        if r.remaining >= 14 {
            guard let repaired = r.readU32(), let nacks = r.readU32(),
                  let audioLost = r.readU32(), let audioBuffer = r.readU16() else { return nil }
            report.fragsRepaired = repaired
            report.nacksSent = nacks
            report.audioPacketsLost = audioLost
            report.audioBufferMs = audioBuffer
        }
        if r.remaining >= 4, let shape = r.readU32() {
            report.pointerShapeId = shape
        }
        return .receiverReport(report)
    }

    /// Enforces what the Rust parser does: a nonzero id, a size within the
    /// cap, the length that size implies, and a chunk inside it.
    private static func parsePointerShape(_ r: inout WireReader) -> ControlMessage? {
        guard r.readU8() == PointerShapeChunk.version,
              let sourceType = r.readU8(), let flags = r.readU8(), let _ = r.readU8(),
              let shapeId = r.readU32(), shapeId != 0,
              let width = r.readU16(), let height = r.readU16(),
              (1...PointerShapeChunk.maxSide).contains(width),
              (1...PointerShapeChunk.maxSide).contains(height),
              let hotX = r.readU16(), let hotY = r.readU16(),
              let totalLen = r.readU32(),
              Int(totalLen) == PointerShapeChunk.imageLength(width: width, height: height, flags: flags),
              let offset = r.readU32(),
              let dataLen = r.readU16(), dataLen > 0,
              Int(offset) + Int(dataLen) <= Int(totalLen),
              let data = r.take(Int(dataLen))
        else { return nil }
        return .pointerShape(PointerShapeChunk(
            shapeId: shapeId, sourceType: sourceType, flags: flags, width: width, height: height,
            hotX: hotX, hotY: hotY, totalLen: totalLen, offset: offset, data: Data(data)))
    }

    private static func parseInputEvent(_ r: inout WireReader) -> ControlMessage? {
        guard let inputVer = r.readU8(),
              let kind = r.readU8(),
              let phase = r.readU8(),
              let buttons = r.readU8(),
              let eventId: UInt32 = r.readU32(),
              let xNorm: UInt16 = r.readU16(),
              let yNorm: UInt16 = r.readU16(),
              let pressureX1000: UInt16 = r.readU16(),
              let scrollDxRaw: UInt16 = r.readU16(),
              let scrollDyRaw: UInt16 = r.readU16(),
              let keycode: UInt16 = r.readU16(),
              let modifiers = r.readU8(),
              let _ = r.readU8(),  // reserved
              let clientTimeUs = r.readU64()
        else { return nil }
        var tiltX: Int16 = 0
        var tiltY: Int16 = 0
        if inputVer == 2 {
            guard let x = r.readU16(), let y = r.readU16() else { return nil }
            tiltX = Int16(bitPattern: x)
            tiltY = Int16(bitPattern: y)
        }
        return .inputEvent(WireInputEvent(
            inputVer: inputVer, kind: kind, phase: phase, buttons: buttons,
            eventId: eventId, xNorm: xNorm, yNorm: yNorm, pressureX1000: pressureX1000,
            scrollDx: Int16(bitPattern: scrollDxRaw), scrollDy: Int16(bitPattern: scrollDyRaw),
            keycode: keycode, modifiers: modifiers, clientTimeUs: clientTimeUs,
            tiltX: tiltX, tiltY: tiltY))
    }
}

// MARK: - Bounds-checked reader

/// Every read either returns the value or nil — indexing can never trap.
private struct WireReader {
    let bytes: [UInt8]
    var at: Int
    var remaining: Int { bytes.count - at }

    mutating func take(_ len: Int) -> ArraySlice<UInt8>? {
        guard at + len <= bytes.count else { return nil }
        defer { at += len }
        return bytes[at..<at + len]
    }

    mutating func readU8() -> UInt8? {
        take(1)?.first
    }

    mutating func readU16() -> UInt16? {
        guard let s = take(2) else { return nil }
        let b = Array(s)
        return UInt16(b[0]) | (UInt16(b[1]) << 8)
    }

    mutating func readU32() -> UInt32? {
        guard let s = take(4) else { return nil }
        let b = Array(s)
        return UInt32(b[0]) | (UInt32(b[1]) << 8) | (UInt32(b[2]) << 16) | (UInt32(b[3]) << 24)
    }

    mutating func readU64() -> UInt64? {
        guard let low = readU32(), let high = readU32() else { return nil }
        return UInt64(low) | (UInt64(high) << 32)
    }

    /// Length-prefixed UTF-8 string, max 64 bytes.
    mutating func readName() -> String? {
        guard let len = readU8(), Int(len) <= Hello2.maxNameLength,
              let slice = take(Int(len))
        else { return nil }
        return String(bytes: slice, encoding: .utf8)
    }
}

// MARK: - Little-endian append helpers

private extension Data {
    mutating func appendLE(_ value: UInt16) {
        append(UInt8(value & 0xFF))
        append(UInt8(value >> 8))
    }

    mutating func appendLE(_ value: UInt32) {
        appendLE(UInt16(value & 0xFFFF))
        appendLE(UInt16(value >> 16))
    }

    mutating func appendLE(_ value: UInt64) {
        appendLE(UInt32(value & 0xFFFF_FFFF))
        appendLE(UInt32(value >> 32))
    }

    /// UTF-8 prefix of `source` that fits `max` bytes without splitting a
    /// character — mirrors the Rust `truncated_name`.
    func prefixOnCharBoundary(_ source: String, max: Int) -> Data {
        if count <= max { return self }
        var end = source.utf8.index(source.utf8.startIndex, offsetBy: max)
        while end != source.utf8.startIndex, !source.isValidUTF8Boundary(end) {
            end = source.utf8.index(before: end)
        }
        return Data(source.utf8[source.utf8.startIndex..<end])
    }
}

private extension String {
    func isValidUTF8Boundary(_ index: String.UTF8View.Index) -> Bool {
        // A UTF-8 boundary is any byte that is not a continuation byte.
        guard index != utf8.endIndex else { return true }
        return utf8[index] & 0b1100_0000 != 0b1000_0000
    }
}

private extension UInt32 {
    init(littleEndianBytes b: [UInt8], at: Int) {
        self = UInt32(b[at]) | (UInt32(b[at + 1]) << 8) | (UInt32(b[at + 2]) << 16)
            | (UInt32(b[at + 3]) << 24)
    }
}
