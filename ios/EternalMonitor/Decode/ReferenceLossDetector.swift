import Foundation

/// Tells, from the reassembler's counters as each frame is delivered, whether
/// a frame was lost or the stream restarted since the previous delivery. Every
/// frame until the next keyframe then references a picture the decoder never
/// had, so `VideoDecoder.referenceLost()` holds the picture instead.
struct ReferenceLossDetector {
    private var previous: FrameAssembler.Counters?

    mutating func lostSincePreviousFrame(_ counters: FrameAssembler.Counters) -> Bool {
        defer { previous = counters }
        guard let previous else { return false }
        return counters.framesDropped != previous.framesDropped
            || counters.streamEpoch != previous.streamEpoch
            // The reassembler zeroes its counters when the stream restarts.
            || counters.framesComplete <= previous.framesComplete
    }
}
