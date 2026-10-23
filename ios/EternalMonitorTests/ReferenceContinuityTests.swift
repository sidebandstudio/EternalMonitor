import XCTest
@testable import EternalMonitor

final class ReferenceContinuityTests: XCTestCase {
    // MARK: - Parsing never traps

    func testScalingListDeltaOutOfRangeIsUnparseableNotATrap() {
        // Sol: a High-profile SPS whose scaling-list delta decodes to Int32.max
        // overflowed `last + delta` and killed the app.
        let sps = Data([0x67, 0x64, 0x00, 0x00, 0xAD, 0x80, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFE])
        XCTAssertNil(ReferenceContinuity.parseH264SPS(sps))
        var continuity = ReferenceContinuity()
        continuity.adoptH264(sps: sps)
        XCTAssertTrue(continuity.admitH264(firstSlice: Data([0x41, 0xFF]), isKeyframe: false, seq: 1, epoch: 1),
                      "a stream that can't be read is never held")
    }

    func testRandomBytesNeverTrapAnyParser() {
        // Seeded, so a failure reproduces.
        var state: UInt64 = 0x9E37_79B9_7F4A_7C15
        func next() -> UInt64 {
            state ^= state << 13; state ^= state >> 7; state ^= state << 17
            return state
        }
        func bytes(header: [UInt8]) -> Data {
            let count = Int(next() % 48)
            // Mostly random, sometimes long runs of ones or zeros, which
            // decode to the largest Exp-Golomb values.
            let fill = next() % 4
            return Data(header + (0..<count).map { _ in
                switch fill {
                case 0: return 0xFF
                case 1: return UInt8(truncatingIfNeeded: next() & 0x01)
                default: return UInt8(truncatingIfNeeded: next())
                }
            })
        }
        let h264Params = [
            ReferenceContinuity.H264Parameters(log2MaxFrameNum: 4, separateColourPlane: false, gapsAllowed: false),
            ReferenceContinuity.H264Parameters(log2MaxFrameNum: 16, separateColourPlane: true, gapsAllowed: false),
        ]
        let hevcParams = [
            ReferenceContinuity.HEVCParameters(log2MaxPocLsb: 4, separateColourPlane: false, extraSliceHeaderBits: 0, outputFlagPresent: false),
            ReferenceContinuity.HEVCParameters(log2MaxPocLsb: 16, separateColourPlane: true, extraSliceHeaderBits: 7, outputFlagPresent: true),
        ]
        var h264 = ReferenceContinuity()
        var hevc = ReferenceContinuity()
        for round in 0..<4_000 {
            let h264SPS = bytes(header: [0x67, [66, 77, 100, 244][round % 4]])
            _ = ReferenceContinuity.parseH264SPS(h264SPS)
            _ = ReferenceContinuity.parseHEVCSPS(bytes(header: [0x42, 0x01]))
            _ = ReferenceContinuity.parseHEVCPPS(bytes(header: [0x44, 0x01]))
            let h264Slice = bytes(header: [[0x41, 0x65, 0x01][round % 3]])
            let hevcSlice = bytes(header: [[0x02, 0x26, 0x2A][round % 3], 0x01])
            for params in h264Params { _ = ReferenceContinuity.h264FrameNum(h264Slice, params) }
            for params in hevcParams { _ = ReferenceContinuity.hevcPocLsb(hevcSlice, params) }
            // And through the stateful path, with random parameter sets.
            if round % 50 == 0 {
                h264.adoptH264(sps: h264SPS)
                hevc.adoptHEVC(sps: bytes(header: [0x42, 0x01]), pps: bytes(header: [0x44, 0x01]))
            }
            let seq = UInt32(truncatingIfNeeded: next())
            let epoch = UInt32(truncatingIfNeeded: next() % 3)
            _ = h264.admitH264(firstSlice: h264Slice, isKeyframe: round % 7 == 0, seq: seq, epoch: epoch)
            _ = hevc.admitHEVC(firstSlice: hevcSlice, isKeyframe: round % 7 == 0, seq: seq, epoch: epoch)
        }
    }

    // MARK: - HEVC picture order

    /// The real x265 parameter sets: 8-bit POC lsb, no extra slice header bits.
    private func hevcContinuity() throws -> ReferenceContinuity {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "gop2_64x64", withExtension: "h265", subdirectory: "Fixtures"))
        let data = try Data(contentsOf: url)
        var nals: [Data] = []
        var starts: [Int] = []
        for i in 0..<(data.count - 2) where data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 { starts.append(i + 3) }
        for (n, start) in starts.enumerated() {
            var end = n + 1 < starts.count ? starts[n + 1] - 3 : data.count
            if end > start, data[end - 1] == 0 { end -= 1 }
            nals.append(data[start..<end])
        }
        let sps = try XCTUnwrap(nals.first { ($0[$0.startIndex] >> 1) & 0x3F == 33 })
        let pps = try XCTUnwrap(nals.first { ($0[$0.startIndex] >> 1) & 0x3F == 34 })
        var continuity = ReferenceContinuity()
        continuity.adoptHEVC(sps: Data(sps), pps: Data(pps))
        return continuity
    }

    /// A first slice with the given POC lsb: an IDR (POC 0) when `poc` is nil.
    private func slice(poc: UInt8?) -> Data {
        var bits: [UInt8] = [1]                          // first_slice_segment_in_pic_flag
        if poc == nil { bits.append(0) }                 // no_output_of_prior_pics_flag
        bits += [1]                                      // slice_pic_parameter_set_id = 0
        bits += poc == nil ? [0, 1, 1] : [0, 1, 0]       // slice_type I (2) or P (1)
        if let poc { bits += (0..<8).map { (poc >> (7 - $0)) & 1 } }
        bits.append(1)                                   // stop bit
        while bits.count % 8 != 0 { bits.append(0) }
        let payload = stride(from: 0, to: bits.count, by: 8).map { i in bits[i..<i + 8].reduce(0) { $0 << 1 | $1 } }
        return Data((poc == nil ? [19 << 1, 0x01] : [1 << 1, 0x01]) + payload)
    }

    private func admit(_ continuity: inout ReferenceContinuity, _ pictures: [(seq: UInt32, poc: UInt8?, epoch: UInt32)]) -> [Bool] {
        pictures.map { continuity.admitHEVC(firstSlice: slice(poc: $0.poc), isKeyframe: $0.poc == nil, seq: $0.seq, epoch: $0.epoch) }
    }

    func testLostPictureIsReadFromALearnedConstantStep() throws {
        var continuity = try hevcContinuity()
        XCTAssertEqual(admit(&continuity, [(1, nil, 1), (2, 1, 1), (4, 3, 1)]), [true, true, false])
    }

    func testNonconstantPictureOrderStopsLossDetectionForTheStream() throws {
        // Sol: POC 0, 1, 3, 4 on seq 1, 2, 3, 5. seq 3 follows seq 2, so
        // nothing was lost, yet the step changed: this stream's POC can't be
        // trusted, and seq 5 must not be held.
        var continuity = try hevcContinuity()
        XCTAssertEqual(admit(&continuity, [(1, nil, 1), (2, 1, 1), (3, 3, 1), (5, 4, 1)]), [true, true, true, true])
        XCTAssertTrue(continuity.pocUnreliable)
        // Whole-frame loss isn't read from this stream's POC any more.
        XCTAssertEqual(admit(&continuity, [(9, 20, 1)]), [true])
        // A new epoch is a new encoder: learn again, and detect again.
        XCTAssertEqual(admit(&continuity, [(1, nil, 2), (2, 1, 2), (4, 3, 2)]), [true, true, false])
        XCTAssertFalse(continuity.pocUnreliable)
    }

    func testNewEpochForgetsTheOldStreamsStep() throws {
        // Epoch 1 stepped by 2; epoch 2's encoder steps by 1. Its first frames
        // after a skipped number must not be judged by the old step.
        var continuity = try hevcContinuity()
        XCTAssertEqual(admit(&continuity, [(1, nil, 1), (2, 2, 1), (3, 4, 1)]), [true, true, true])
        XCTAssertEqual(admit(&continuity, [(4, nil, 2), (6, 1, 2), (7, 2, 2)]), [true, true, true])
    }

    func testNewEpochWithoutItsKeyframeIsHeld() throws {
        // The new stream's keyframe was lost; its next frame follows on from
        // the old stream's numbers by chance, but can't follow its picture.
        var continuity = try hevcContinuity()
        XCTAssertEqual(admit(&continuity, [(1, nil, 1), (2, 1, 2), (3, nil, 2), (4, 1, 2)]), [true, false, true, true])
    }

    // MARK: - Contiguous numbering and restarts within an epoch

    func testContiguousNumberingHoldsTheFrameAfterALostEncoderReopenKeyframe() throws {
        // Sol: the PC reopened its encoder (same epoch, numbering continues),
        // and the new encoder's keyframe, seq 2, was lost. Its first P-frame
        // has POC 1, which follows the old keyframe by chance.
        var numbered = try hevcContinuity()
        numbered.frameSeqContiguous = true
        XCTAssertEqual(admit(&numbered, [(1, nil, 1), (3, 1, 1)]), [true, false])
        // A 0.3.1 PC numbers as captured, so a gap there means nothing.
        var captured = try hevcContinuity()
        XCTAssertEqual(admit(&captured, [(1, nil, 1), (3, 1, 1)]), [true, true])
    }

    func testContiguousNumberingNeverHoldsAnUnbrokenStream() throws {
        var continuity = try hevcContinuity()
        continuity.frameSeqContiguous = true
        XCTAssertEqual(admit(&continuity, [(1, nil, 1), (2, 1, 1), (3, 2, 1), (4, 3, 1)]), [true, true, true, true])
        // A keyframe after a gap is always shown, and ends the gap.
        XCTAssertEqual(admit(&continuity, [(7, nil, 1), (8, 1, 1)]), [true, true])
    }

    func testLowerFrameSeqWithinAnEpochIsARestart() throws {
        // Sol: step 2 learned on seq 1000-1002; the numbering restarts at 1
        // with a keyframe, then a valid POC 1 after a skipped number.
        var continuity = try hevcContinuity()
        XCTAssertEqual(admit(&continuity, [(1000, nil, 1), (1001, 2, 1), (1002, 4, 1)]), [true, true, true])
        XCTAssertEqual(admit(&continuity, [(1, nil, 1), (3, 1, 1)]), [true, true])
        // Without its keyframe the restarted stream is held.
        var lost = try hevcContinuity()
        XCTAssertEqual(admit(&lost, [(1000, nil, 1), (1001, 2, 1), (5, 6, 1)]), [true, true, false])
    }
}
