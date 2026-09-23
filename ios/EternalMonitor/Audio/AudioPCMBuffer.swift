import AVFoundation
import Foundation
import os

/// The render callback only copies samples and updates scalar counters under
/// this lock. Codec work, allocations, session changes and logging stay off it.
final class AudioPCMBuffer {
    static let packetFrames = 960
    private static let capacity = 48 * 140
    struct Snapshot {
        var bufferMs = 0
        var targetMs = 60
        var underruns: UInt64 = 0
        var overruns: UInt64 = 0
        var renderedFrames: UInt64 = 0
        var rms = 0.0
        var tone1kDB = -120.0
    }
    private struct State {
        var pcm = [Float](repeating: 0, count: capacity * 2)
        var head = 0
        var count = 0
        var primed = false
        var hasPlayed = false
        var silenceSinceWrite = 0
        var targetMs = 60
        var lastUnderrunUs: UInt64?
        var underruns: UInt64 = 0
        var overruns: UInt64 = 0
        var renderedFrames: UInt64 = 0
        var meterFrames = 0
        var q1 = 0.0
        var q2 = 0.0
        var energy = 0.0
        var rms = 0.0
        var tone1kDB = -120.0

        mutating func discard(_ frames: Int) {
            let n = min(count, frames)
            head = (head + n) % capacity
            count -= n
        }
        mutating func adapt(nowUs: UInt64) {
            if targetMs == 120, let last = lastUnderrunUs,
               nowUs >= last, nowUs - last >= 30_000_000 {
                targetMs = 60
                while count > 48 * 80 { discard(packetFrames) }
            }
        }
        mutating func write(left: Float, right: Float) {
            if count == min(capacity, (targetMs + 20) * 48) {
                discard(packetFrames)
                overruns += 1
            }
            let tail = (head + count) % capacity
            pcm[tail * 2] = left
            pcm[tail * 2 + 1] = right
            count += 1
        }
    }
    private let state = OSAllocatedUnfairLock(initialState: State())
    private let meterEnabled: Bool
    private static let toneCoefficient = 2 * cos(2 * Double.pi * 1_000 / 48_000)

    init(meterEnabled: Bool = false) { self.meterEnabled = meterEnabled }

    func reset() {
        state.withLock { s in
            // Retain allocated sample storage; clearing count makes old audio unreachable.
            s.head = 0; s.count = 0; s.primed = false; s.hasPlayed = false
            s.silenceSinceWrite = 0; s.meterFrames = 0; s.q1 = 0; s.q2 = 0; s.energy = 0
            s.rms = 0; s.tone1kDB = -120
        }
    }

    func append(_ samples: [Float]) {
        precondition(samples.count == Self.packetFrames * 2)
        state.withLock { s in
            for frame in 0..<Self.packetFrames {
                s.write(left: samples[frame * 2], right: samples[frame * 2 + 1])
            }
            s.silenceSinceWrite = 0
        }
    }

    /// Silence already emitted by the renderer during a DTX interval counts
    /// toward that interval; do not play the same silence a second time.
    func appendSilence(frames: Int) {
        state.withLock { s in
            let remaining = max(0, frames - s.silenceSinceWrite)
            s.silenceSinceWrite = max(0, s.silenceSinceWrite - frames)
            for _ in 0..<min(Self.capacity, remaining) { s.write(left: 0, right: 0) }
        }
    }

    var snapshot: Snapshot {
        state.withLock { s in
            Snapshot(bufferMs: s.count / 48, targetMs: s.targetMs, underruns: s.underruns,
                     overruns: s.overruns, renderedFrames: s.renderedFrames,
                     rms: s.rms, tone1kDB: s.tone1kDB)
        }
    }

    /// AVAudioSourceNode uses a noninterleaved stereo Float32 format.
    func render(frames: Int, buffers: UnsafeMutableAudioBufferListPointer, nowUs: UInt64) {
        guard buffers.count == 2,
              let left = buffers[0].mData?.assumingMemoryBound(to: Float.self),
              let right = buffers[1].mData?.assumingMemoryBound(to: Float.self) else { return }
        state.withLock { s in
            s.adapt(nowUs: nowUs)
            if !s.primed, s.count >= s.targetMs * 48 { s.primed = true; s.hasPlayed = true }
            for i in 0..<frames {
                var l: Float = 0
                var r: Float = 0
                if s.primed, s.count > 0 {
                    l = s.pcm[s.head * 2]; r = s.pcm[s.head * 2 + 1]
                    s.discard(1)
                } else {
                    if s.primed {
                        s.underruns += 1
                        if let last = s.lastUnderrunUs, nowUs >= last, nowUs - last <= 10_000_000 {
                            s.targetMs = 120
                        }
                        s.lastUnderrunUs = nowUs
                        s.primed = false
                    }
                    if s.hasPlayed { s.silenceSinceWrite = min(Self.capacity, s.silenceSinceWrite + 1) }
                }
                left[i] = l; right[i] = r
                if meterEnabled {
                    let mono = Double(l + r) * 0.5
                    let q = mono + Self.toneCoefficient * s.q1 - s.q2
                    s.q2 = s.q1; s.q1 = q
                    s.energy += mono * mono
                    s.meterFrames += 1
                    if s.meterFrames == 4_800 {
                        s.rms = sqrt(s.energy / 4_800)
                        let power = max(0, s.q1 * s.q1 + s.q2 * s.q2 - Self.toneCoefficient * s.q1 * s.q2)
                        s.tone1kDB = 20 * log10(max(0.000001, 2 * sqrt(power) / 4_800))
                        s.q1 = 0; s.q2 = 0; s.energy = 0; s.meterFrames = 0
                    }
                }
            }
            s.renderedFrames += UInt64(frames)
        }
    }
}
