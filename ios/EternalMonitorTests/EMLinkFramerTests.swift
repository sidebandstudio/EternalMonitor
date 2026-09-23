import XCTest
@testable import EternalMonitor

final class EMLinkFramerTests: XCTestCase {
    func testEverySplitPreservesPreambleAndPacketBoundaries() throws {
        let packets = [Data([0xA1]), Data(repeating: 0xBC, count: 1400), Data([1, 2, 3, 4])]
        var stream = EMLinkFramer.preamble
        for packet in packets { stream.append(try EMLinkFramer.encode(packet)) }
        for split in 0...stream.count {
            var parser = EMLinkFramer()
            var decoded = try parser.append(stream.prefix(split))
            XCTAssertLessThanOrEqual(parser.bufferedBytes, EMLinkFramer.maximumLength)
            decoded += try parser.append(stream.suffix(from: split))
            XCTAssertEqual(decoded, packets)
            XCTAssertTrue(parser.preambleReceived)
            XCTAssertEqual(parser.bufferedBytes, 0)
        }
    }

    func testOneByteReadsAndWriterWithoutPreamble() throws {
        let packet = Data(repeating: 0xDA, count: 64)
        let encoded = try EMLinkFramer.encode(packet)
        XCTAssertEqual(encoded.prefix(2), Data([64, 0]))
        var parser = EMLinkFramer(expectsPreamble: false)
        var packets: [Data] = []
        for byte in encoded { packets += try parser.append(Data([byte])) }
        XCTAssertEqual(packets, [packet])
    }

    func testBadPreambleClosesParser() {
        for changedByte in 0..<8 {
            var preamble = EMLinkFramer.preamble
            preamble[changedByte] ^= 0xFF
            var parser = EMLinkFramer()
            XCTAssertThrowsError(try parser.append(preamble)) {
                XCTAssertEqual($0 as? EMLinkFramer.FramingError, .invalidPreamble)
            }
            XCTAssertThrowsError(try parser.append(EMLinkFramer.preamble)) {
                XCTAssertEqual($0 as? EMLinkFramer.FramingError, .closed)
            }
        }
    }

    func testInvalidLengthsFailBeforeAllocatingTheirBodies() {
        for length in [0, 1401, 65535] {
            var parser = EMLinkFramer(expectsPreamble: false)
            XCTAssertThrowsError(try parser.append(Data([UInt8(length & 0xFF), UInt8(length >> 8)]))) {
                XCTAssertEqual($0 as? EMLinkFramer.FramingError, .invalidLength)
            }
            XCTAssertEqual(parser.bufferedBytes, 0)
        }
        for length in [0, 1401] {
            XCTAssertThrowsError(try EMLinkFramer.encode(Data(repeating: 0, count: length)))
        }
    }
}
