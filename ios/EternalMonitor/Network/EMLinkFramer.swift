import Foundation

/// Incremental EMLINK v1 framing. The retained buffer never exceeds one
/// datagram, regardless of how TCP divides or combines the incoming bytes.
struct EMLinkFramer {
    static let preamble = Data([0x45, 0x4D, 0x4C, 0x49, 0x4E, 0x4B, 1, 0])
    static let maximumLength = 1400

    enum FramingError: Error, Equatable {
        case invalidPreamble
        case invalidLength
        case closed
    }

    private var awaitingPreamble: Bool
    private var length: Int?
    private var buffer = Data()
    private var failed = false
    var bufferedBytes: Int { buffer.count }
    var preambleReceived: Bool { !awaitingPreamble && !failed }

    init(expectsPreamble: Bool = true) {
        awaitingPreamble = expectsPreamble
    }

    mutating func append(_ bytes: Data) throws -> [Data] {
        guard !failed else { throw FramingError.closed }
        var packets: [Data] = []
        var cursor = bytes.startIndex
        do {
            while cursor < bytes.endIndex {
                let needed = awaitingPreamble ? Self.preamble.count : (length ?? 2)
                let count = min(needed - buffer.count, bytes.distance(from: cursor, to: bytes.endIndex))
                let end = bytes.index(cursor, offsetBy: count)
                buffer.append(contentsOf: bytes[cursor..<end])
                cursor = end
                guard buffer.count == needed else { break }
                if awaitingPreamble {
                    guard buffer == Self.preamble else { throw FramingError.invalidPreamble }
                    awaitingPreamble = false
                    buffer.removeAll(keepingCapacity: true)
                } else if length == nil {
                    let count = Int(buffer[buffer.startIndex]) | Int(buffer[buffer.index(after: buffer.startIndex)]) << 8
                    guard (1...Self.maximumLength).contains(count) else { throw FramingError.invalidLength }
                    length = count
                    buffer.removeAll(keepingCapacity: true)
                } else {
                    packets.append(buffer)
                    buffer = Data()
                    length = nil
                }
            }
            return packets
        } catch {
            failed = true
            buffer.removeAll()
            throw error
        }
    }

    static func encode(_ datagram: Data) throws -> Data {
        guard (1...maximumLength).contains(datagram.count) else { throw FramingError.invalidLength }
        var bytes = Data([UInt8(datagram.count & 0xFF), UInt8(datagram.count >> 8)])
        bytes.append(datagram)
        return bytes
    }
}
