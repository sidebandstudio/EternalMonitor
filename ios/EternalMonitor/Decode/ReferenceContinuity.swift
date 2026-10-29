import Foundation

/// Tells, from the bitstream itself, whether a picture follows a lost
/// reference picture, so the decoder can hold the last good picture instead of
/// drawing smeared blocks.
///
/// The wire's `frame_seq` can't tell: the host numbers frames as captured, and
/// its capture-to-encoder handoff drops frames the encoder had no time for, so
/// numbers skip although nothing the iPad needs was lost. The codec's own
/// counters count coded pictures only:
/// - H.264 `frame_num` advances by one after every reference picture, modulo
///   MaxFrameNum, so a lost reference shows as a jump. Exact.
/// - HEVC has no such counter. Its POC advances by a fixed step per picture on
///   NVENC, AMF and x265 (checked against their streams, frames dropped before
///   encoding included). The step is learned only between two frames whose
///   `frame_seq` is contiguous, which proves nothing vanished in between. If a
///   later contiguous pair disagrees, the stream's POC isn't constant, and
///   whole-frame loss isn't read from it again until the stream restarts.
///
/// A PC that numbers sent frames contiguously per epoch (0.3.4 and later,
/// HELLO_ACK host cap `HelloAck.hostCapContiguousFrameSeq`) makes any forward
/// gap in `frame_seq` a lost frame, whatever the codec says. That also covers
/// a lost keyframe after the PC reopened its encoder (bitrate, size, codec, an
/// AMF keyframe), which keeps the epoch and the numbering but starts the
/// codec's counts again.
///
/// A new stream epoch is a new encoder: its pictures can't follow the old
/// stream's, so it starts again from its keyframe. So does a lower `frame_seq`
/// within an epoch, which a PC sends only after restarting its numbering.
/// A picture this cannot read (an unexpected or malformed stream) is never
/// treated as a loss: holding by mistake freezes a good picture. Parsing never
/// traps on any input.
struct ReferenceContinuity {
    /// Set from the session's host caps: a forward gap in `frame_seq` is a loss.
    var frameSeqContiguous = false

    struct H264Parameters: Equatable {
        var log2MaxFrameNum: Int
        var separateColourPlane: Bool
        var gapsAllowed: Bool
    }

    struct HEVCParameters: Equatable {
        var log2MaxPocLsb: Int
        var separateColourPlane: Bool
        var extraSliceHeaderBits: Int
        var outputFlagPresent: Bool
    }

    private var h264: H264Parameters?
    private var hevc: HEVCParameters?
    /// H.264: `frame_num` of the latest reference picture.
    private var previousReferenceFrameNum: UInt32?
    /// HEVC: POC lsb of the latest picture, and the learned step between pictures.
    private var previousPoc: UInt32?
    private var pocStep: UInt32?
    /// HEVC: a contiguous pair disagreed with the learned step, so this
    /// stream's POC can't show a lost picture.
    private(set) var pocUnreliable = false
    private var previousSeq: UInt32?
    private var previousEpoch: UInt32?

    // MARK: - Parameter sets

    /// New parameter sets start the count again; the access unit that carries
    /// them is a keyframe.
    mutating func adoptH264(sps: Data?) {
        let parsed = sps.flatMap(Self.parseH264SPS)
        if parsed != h264 { previousReferenceFrameNum = nil }
        h264 = parsed
    }

    mutating func adoptHEVC(sps: Data?, pps: Data?) {
        var parsed: HEVCParameters?
        if let sps, let pps, let s = Self.parseHEVCSPS(sps), let p = Self.parseHEVCPPS(pps) {
            parsed = HEVCParameters(log2MaxPocLsb: s.log2MaxPocLsb, separateColourPlane: s.separateColourPlane,
                                    extraSliceHeaderBits: p.extraBits, outputFlagPresent: p.outputFlagPresent)
        }
        if parsed != hevc {
            previousPoc = nil
            pocStep = nil
        }
        hevc = parsed
    }

    // MARK: - Pictures

    /// Whether the decoder may show this picture. A keyframe always may, and
    /// restarts the count. Any other picture may unless it follows a lost
    /// reference; only shown pictures advance the count. `seq` is the wire's
    /// frame number, `epoch` its stream epoch; a lower `seq` or another epoch
    /// means the host restarted its stream.
    mutating func admitH264(firstSlice nal: Data, isKeyframe: Bool, seq: UInt32, epoch: UInt32) -> Bool {
        let (restarted, gap) = noteStream(seq: seq, epoch: epoch)
        guard let params = h264, let frameNum = Self.h264FrameNum(nal, params) else {
            previousReferenceFrameNum = nil
            return isKeyframe || !(restarted || gap)
        }
        let isReference = (nal[nal.startIndex] >> 5) & 0x03 != 0
        if !isKeyframe {
            if restarted || gap { return false }
            if let previous = previousReferenceFrameNum, !params.gapsAllowed {
                let maxFrameNum = UInt32(1) << params.log2MaxFrameNum
                guard frameNum == (previous + 1) % maxFrameNum else { return false }
            }
        }
        if isReference { previousReferenceFrameNum = frameNum }
        return true
    }

    mutating func admitHEVC(firstSlice nal: Data, isKeyframe: Bool, seq: UInt32, epoch: UInt32) -> Bool {
        let contiguous = previousSeq.map { seq == $0 &+ 1 } ?? false
        let (restarted, gap) = noteStream(seq: seq, epoch: epoch)
        guard let params = hevc, let poc = Self.hevcPocLsb(nal, params) else {
            previousPoc = nil
            return isKeyframe || !(restarted || gap)
        }
        if !isKeyframe {
            if restarted || gap { return false }
            if let previous = previousPoc, !pocUnreliable {
                let delta = (poc &- previous) & ((UInt32(1) << params.log2MaxPocLsb) - 1)
                if contiguous {
                    if pocStep == nil { pocStep = delta } else if delta != pocStep { pocUnreliable = true }
                } else if let step = pocStep, delta != step {
                    return false
                }
            }
        }
        previousPoc = poc
        return true
    }

    /// Records the wire frame number and epoch. `restarted`: another epoch,
    /// or a lower number in this one, so a new encoder whose counts, POC step
    /// and reliability start over. `gap`: a number skipped on a PC that
    /// numbers contiguously.
    private mutating func noteStream(seq: UInt32, epoch: UInt32) -> (restarted: Bool, gap: Bool) {
        defer { previousSeq = seq; previousEpoch = epoch }
        guard let previous = previousSeq else { return (false, false) }
        guard previousEpoch == epoch, seq >= previous else {
            previousReferenceFrameNum = nil
            previousPoc = nil
            pocStep = nil
            pocUnreliable = false
            return (true, false)
        }
        return (false, frameSeqContiguous && seq > previous &+ 1)
    }

    // MARK: - Parsing
    //
    // Every Exp-Golomb value is checked against the range H.264 (7.4.2) and
    // HEVC (7.4.3) allow before it is used; anything else is "can't parse".

    private static let h264HighProfiles: Set<UInt32> = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135]

    static func parseH264SPS(_ nal: Data) -> H264Parameters? {
        var r = H264BitReader(bytes: rbsp(nal, headerBytes: 1))
        guard let profile = r.readBits(8), r.readBits(16) != nil,
              let spsID = r.readUE(), spsID <= 31 else { return nil }
        var separateColourPlane = false
        if h264HighProfiles.contains(profile) {
            guard let chroma = r.readUE(), chroma <= 3 else { return nil }
            if chroma == 3 {
                guard let flag = r.readBit() else { return nil }
                separateColourPlane = flag == 1
            }
            guard let lumaDepth = r.readUE(), lumaDepth <= 6, let chromaDepth = r.readUE(), chromaDepth <= 6,
                  r.readBit() != nil, let scalingPresent = r.readBit() else { return nil }
            if scalingPresent == 1 {
                for list in 0..<(chroma == 3 ? 12 : 8) {
                    guard let present = r.readBit() else { return nil }
                    if present == 1 && !skipScalingList(&r, size: list < 6 ? 16 : 64) { return nil }
                }
            }
        }
        guard let log2MaxFrameNumMinus4 = r.readUE(), log2MaxFrameNumMinus4 <= 12,
              let pocType = r.readUE(), pocType <= 2 else { return nil }
        switch pocType {
        case 0:
            guard let log2MaxPocLsbMinus4 = r.readUE(), log2MaxPocLsbMinus4 <= 12 else { return nil }
        case 1:
            // offset_for_non_ref_pic, offset_for_top_to_bottom_field and each
            // offset_for_ref_frame lie in [-2^31 + 1, 2^31 - 1]; readSE can't
            // return Int32.min, so only the cycle length needs a bound.
            guard r.readBit() != nil, r.readSE() != nil, r.readSE() != nil,
                  let cycle = r.readUE(), cycle <= 255 else { return nil }
            for _ in 0..<cycle { guard r.readSE() != nil else { return nil } }
        default:
            break
        }
        guard let maxRefFrames = r.readUE(), maxRefFrames <= 16, let gaps = r.readBit() else { return nil }
        return H264Parameters(log2MaxFrameNum: Int(log2MaxFrameNumMinus4) + 4,
                              separateColourPlane: separateColourPlane, gapsAllowed: gaps == 1)
    }

    private static func skipScalingList(_ r: inout H264BitReader, size: Int) -> Bool {
        var last: Int32 = 8
        var next: Int32 = 8
        for _ in 0..<size {
            if next != 0 {
                guard let delta = r.readSE(), (-128...127).contains(delta) else { return false }
                next = (last + delta + 256) % 256
            }
            if next != 0 { last = next }
        }
        return true
    }

    static func h264FrameNum(_ nal: Data, _ params: H264Parameters) -> UInt32? {
        var r = H264BitReader(bytes: rbsp(nal, headerBytes: 1))
        guard r.readUE() != nil, let sliceType = r.readUE(), sliceType <= 9,
              let ppsID = r.readUE(), ppsID <= 255 else { return nil }
        if params.separateColourPlane { guard r.readBits(2) != nil else { return nil } }
        return r.readBits(params.log2MaxFrameNum)
    }

    static func parseHEVCSPS(_ nal: Data) -> (log2MaxPocLsb: Int, separateColourPlane: Bool)? {
        var r = H264BitReader(bytes: rbsp(nal, headerBytes: 2))
        guard r.readBits(4) != nil, let maxSubLayersMinus1 = r.readBits(3), maxSubLayersMinus1 <= 6,
              r.readBit() != nil else { return nil }
        // profile_tier_level: 88 bits of general profile, 8 of general level.
        guard r.skip(96) else { return nil }
        var subLayerProfile: [Bool] = []
        var subLayerLevel: [Bool] = []
        for _ in 0..<Int(maxSubLayersMinus1) {
            guard let profile = r.readBit(), let level = r.readBit() else { return nil }
            subLayerProfile.append(profile == 1)
            subLayerLevel.append(level == 1)
        }
        if maxSubLayersMinus1 > 0 { guard r.skip(2 * (8 - Int(maxSubLayersMinus1))) else { return nil } }
        for i in 0..<Int(maxSubLayersMinus1) {
            guard r.skip((subLayerProfile[i] ? 88 : 0) + (subLayerLevel[i] ? 8 : 0)) else { return nil }
        }
        guard let spsID = r.readUE(), spsID <= 15, let chroma = r.readUE(), chroma <= 3 else { return nil }
        var separateColourPlane = false
        if chroma == 3 {
            guard let flag = r.readBit() else { return nil }
            separateColourPlane = flag == 1
        }
        guard let width = r.readUE(), width > 0, let height = r.readUE(), height > 0,
              let window = r.readBit() else { return nil }
        if window == 1 { for _ in 0..<4 { guard r.readUE() != nil else { return nil } } }
        guard let lumaDepth = r.readUE(), lumaDepth <= 8, let chromaDepth = r.readUE(), chromaDepth <= 8,
              let log2MaxPocLsbMinus4 = r.readUE(), log2MaxPocLsbMinus4 <= 12 else { return nil }
        return (Int(log2MaxPocLsbMinus4) + 4, separateColourPlane)
    }

    static func parseHEVCPPS(_ nal: Data) -> (extraBits: Int, outputFlagPresent: Bool)? {
        var r = H264BitReader(bytes: rbsp(nal, headerBytes: 2))
        guard let ppsID = r.readUE(), ppsID <= 63, let spsID = r.readUE(), spsID <= 15, r.readBit() != nil,
              let outputFlag = r.readBit(), let extraBits = r.readBits(3) else { return nil }
        return (Int(extraBits), outputFlag == 1)
    }

    static func hevcPocLsb(_ nal: Data, _ params: HEVCParameters) -> UInt32? {
        guard nal.count >= 2 else { return nil }
        let nalType = (nal[nal.startIndex] >> 1) & 0x3F
        var r = H264BitReader(bytes: rbsp(nal, headerBytes: 2))
        guard let firstSliceInPicture = r.readBit(), firstSliceInPicture == 1 else { return nil }
        if (16...23).contains(nalType) { guard r.readBit() != nil else { return nil } }
        guard let ppsID = r.readUE(), ppsID <= 63, r.skip(params.extraSliceHeaderBits),
              let sliceType = r.readUE(), sliceType <= 2 else { return nil }
        if params.outputFlagPresent { guard r.readBit() != nil else { return nil } }
        if params.separateColourPlane { guard r.readBits(2) != nil else { return nil } }
        // IDR pictures carry no POC; it is zero.
        if nalType == 19 || nalType == 20 { return 0 }
        return r.readBits(params.log2MaxPocLsb)
    }

    /// The NAL payload after its header, without emulation-prevention bytes.
    /// Slice and parameter-set headers sit at the start, so a prefix suffices.
    private static func rbsp(_ nal: Data, headerBytes: Int) -> [UInt8] {
        var out: [UInt8] = []
        out.reserveCapacity(64)
        var zeros = 0
        for byte in nal.dropFirst(headerBytes).prefix(256) {
            if zeros >= 2 && byte == 0x03 { zeros = 0; continue }
            out.append(byte)
            zeros = byte == 0 ? zeros + 1 : 0
        }
        return out
    }
}
