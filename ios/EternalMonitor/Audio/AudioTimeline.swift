import Foundation

struct ReceivedAudio {
    var header: AudioHeader
    let opus: Data
    let receivedUs: UInt64
}

/// Bounded packet reordering ahead of the PCM ring. This object and its codec
/// belong to one processing queue; no decoder runs on the render callback.
final class AudioTimeline {
    private let decoder: OpusDecoder
    let pcm: AudioPCMBuffer
    private var pending: [UInt32: ReceivedAudio] = [:]
    private var epoch: UInt32?
    private var nextSeq: UInt32?
    private var lastCaptureUs: UInt64?
    private var gapSinceUs: UInt64?
    private(set) var packets: UInt64 = 0
    private(set) var decoded: UInt64 = 0
    private(set) var lost: UInt64 = 0
    private(set) var concealed: UInt64 = 0
    private(set) var stale: UInt64 = 0
    var pendingCount: Int { pending.count }

    init(pcm: AudioPCMBuffer) throws { decoder = try OpusDecoder(); self.pcm = pcm }

    func receive(_ packet: ReceivedAudio) throws {
        let h = packet.header
        if let epoch, epoch != h.streamEpoch {
            guard Int32(bitPattern: h.streamEpoch &- epoch) > 0 else { stale += 1; return }
            try restart(at: h)
        } else if epoch == nil {
            try restart(at: h)
        } else if h.discontinuity, let nextSeq,
                  Int32(bitPattern: h.audioSeq &- nextSeq) < 0,
                  h.captureTimestampUs > (lastCaptureUs ?? 0) {
            // A live off/on toggle restarts audio sequence numbers in the same
            // video epoch. A late old first packet cannot rewind the timeline.
            try restart(at: h)
        }
        guard let nextSeq, Int32(bitPattern: h.audioSeq &- nextSeq) >= 0,
              pending[h.audioSeq] == nil else { stale += 1; return }
        packets += 1
        if pending.count == 8 {
            // Keep the closest packets; a distant future packet must not evict
            // one that is about to play. Normal loss accounting occurs on drain.
            let furthest = pending.keys.max { ($0 &- nextSeq) < ($1 &- nextSeq) }!
            guard h.audioSeq &- nextSeq < furthest &- nextSeq else { return }
            pending.removeValue(forKey: furthest)
        }
        pending[h.audioSeq] = packet
    }

    private func restart(at header: AudioHeader) throws {
        pending.removeAll(keepingCapacity: true)
        epoch = header.streamEpoch
        nextSeq = header.audioSeq
        lastCaptureUs = nil
        gapSinceUs = nil
        try decoder.reset()
        pcm.reset()
    }

    func drain(nowUs: UInt64) throws {
        // Eight packets and at most six PLC frames per turn bound work even
        // when a peer advertises a sequence number billions of frames ahead.
        for _ in 0..<14 {
            guard let expected = nextSeq else { return }
            if let packet = pending.removeValue(forKey: expected) {
                gapSinceUs = nil
                let h = packet.header
                if let last = lastCaptureUs, h.captureTimestampUs > last,
                   h.captureTimestampUs - last > 25_000 {
                    let silenceUs = h.captureTimestampUs - last - 20_000
                    if silenceUs > 140_000 { pcm.reset(); try decoder.reset() }
                    else { pcm.appendSilence(frames: Int(silenceUs * 48 / 1_000)) }
                }
                // Encoder resets at silence/reopen keep sequence and clock
                // continuity. Reset the codec without throwing away queued PCM.
                if h.discontinuity { try decoder.reset() }
                do {
                    pcm.append(try decoder.decode(packet.opus))
                    decoded += 1
                } catch {
                    pcm.append(try decoder.decode(nil))
                    lost += 1; concealed += 1
                }
                lastCaptureUs = h.captureTimestampUs
                nextSeq = expected &+ 1
            } else {
                guard let nearest = pending.keys.min(by: { ($0 &- expected) < ($1 &- expected) }) else { return }
                if gapSinceUs == nil { gapSinceUs = pending.values.map(\.receivedUs).min() ?? nowUs }
                guard let since = gapSinceUs, nowUs >= since, nowUs - since >= 20_000 else { return }
                let missing = nearest &- expected
                if missing > 6 {
                    lost += UInt64(missing)
                    try decoder.reset()
                    pcm.reset()
                    lastCaptureUs = nil
                    nextSeq = nearest
                } else {
                    pcm.append(try decoder.decode(nil))
                    lost += 1; concealed += 1
                    if let last = lastCaptureUs { lastCaptureUs = last &+ 20_000 }
                    nextSeq = expected &+ 1
                }
            }
        }
    }
}
