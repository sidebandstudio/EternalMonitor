import XCTest
@testable import EternalMonitor

final class OpusDecoderTests: XCTestCase {
    private func fixture() throws -> Data {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(
            forResource: "tone_1khz_20ms", withExtension: "opus", subdirectory: "Fixtures"
        ))
        return try Data(contentsOf: url)
    }

    func testDecodesHostOpusPacketToStereoPCM() throws {
        let decoder = try OpusDecoder()
        let pcm = try decoder.decode(fixture())
        XCTAssertEqual(pcm.count, 1920)
        XCTAssertTrue(pcm.allSatisfy(\.isFinite))
        let rms = sqrt(pcm.reduce(0.0) { $0 + Double($1 * $1) } / Double(pcm.count))
        XCTAssertGreaterThan(rms, 0.05)
        XCTAssertLessThan(rms, 0.3)
    }

    func testMissingPacketProducesConcealmentAndResetAcceptsAnotherPacket() throws {
        let decoder = try OpusDecoder()
        let packet = try fixture()
        _ = try decoder.decode(packet)
        let concealed = try decoder.decode(nil)
        XCTAssertEqual(concealed.count, 1920)
        XCTAssertTrue(concealed.allSatisfy(\.isFinite))
        try decoder.reset()
        XCTAssertEqual(try decoder.decode(packet).count, 1920)
    }
}
