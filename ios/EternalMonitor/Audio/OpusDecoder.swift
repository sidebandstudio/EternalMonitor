import Copus
import Foundation

enum OpusDecoderError: Error {
    case codec(Int32)
    case invalidPacket
    case frameCount(Int32)
}

/// One libopus state per audio session, confined to the audio processing queue.
/// The source package supplies the same decoder on devices and simulators.
final class OpusDecoder {
    private let decoder: OpaquePointer

    init() throws {
        var status: Int32 = OPUS_OK
        guard let created = opus_decoder_create(48_000, 2, &status), status == OPUS_OK else {
            throw OpusDecoderError.codec(status)
        }
        decoder = created
    }

    deinit { opus_decoder_destroy(decoder) }

    func reset() throws {
        let status = opus_decoder_init(decoder, 48_000, 2)
        guard status == OPUS_OK else { throw OpusDecoderError.codec(status) }
    }

    /// Nil asks libopus for exactly one missing 20 ms frame (PLC).
    func decode(_ packet: Data?) throws -> [Float] {
        if let packet, !(1...1372).contains(packet.count) { throw OpusDecoderError.invalidPacket }
        var samples = [Float](repeating: 0, count: 960 * 2)
        let frames: Int32 = try (packet ?? Data()).withUnsafeBytes { bytes in
            let input = bytes.isEmpty ? nil : bytes.bindMemory(to: UInt8.self).baseAddress
            if let input {
                let duration = opus_packet_get_nb_samples(input, Int32(bytes.count), 48_000)
                guard duration == 960 else { throw OpusDecoderError.invalidPacket }
            }
            return opus_decode_float(decoder, input, Int32(bytes.count), &samples, 960, 0)
        }
        guard frames >= 0 else { throw OpusDecoderError.codec(frames) }
        guard frames == 960 else { throw OpusDecoderError.frameCount(frames) }
        return samples
    }
}
