import Foundation
import os

/// Reassembles fragmented UDP datagrams into complete Annex B access units.
/// Called exclusively from the UDP receiver's serial queue — no locking needed.
///
/// Hardened against malformed/hostile input: fragment counts, in-flight frame
/// counts, and total buffered bytes are all capped, so no packet sequence can
/// balloon memory. (`eternal-wire`'s `reassembly.rs` mirrors these semantics
/// for the host-side tests; this file is the specification.)
final class FrameAssembler {
    /// Completed access unit: raw Annex B payload + the frame metadata every
    /// fragment carried (protocol v2 repeats it per datagram).
    var onFrameAssembled: ((Data, _ seq: UInt32, _ captureTimestampUs: UInt64, _ isKeyframe: Bool) -> Void)?
    var onDiagnostic: ((String) -> Void)?
    var onNack: ((Nack) -> Void)?
    var onNeedsKeyframe: ((_ epoch: UInt32, _ lastCompleteSeq: UInt32) -> Void)?
    var repairEnabled = false
    var framePeriodUs: UInt64 = 16_667
    var rttUs: () -> UInt64 = { 10_000 }
    private let nowUs: () -> UInt64

    init(nowUs: @escaping () -> UInt64 = ControlChannel.clientNowUs) {
        self.nowUs = nowUs
    }

    /// A frame may span at most this many fragments. This MUST match the
    /// protocol cap the host fragments against and the wire parser enforces:
    /// a tighter value here silently discards legitimately large access units
    /// (a 1440p/4K scene-change IDR runs past 1024 fragments), and because the
    /// decoder then never receives a sync sample, every later frame is dropped
    /// too — a permanent freeze with healthy heartbeats. `maxPendingBytes` is
    /// the real memory guard; the u16 field alone would allow ≈90 MB.
    static let maxFragmentCount: UInt16 = MediaHeader.maxFragCount
    /// At most this many frames in flight; the oldest is dropped first. The
    /// repair window holds completed frames behind an incomplete one under
    /// the same bound. WiFi reorders fragments by a few milliseconds and
    /// releases stalled frames in bursts, so a tighter bound evicted frames
    /// just before their last fragment arrived. The repair deadline, not the
    /// frame count, limits how long a gap may delay newer frames.
    static let maxPendingFrames = 8
    /// Hard ceiling on buffered fragment bytes across all partial frames.
    static let maxPendingBytes = 8 * 1024 * 1024
    /// WiFi can hold traffic in either direction for about 100 ms and then
    /// release it at once. Dropping a frame then gains nothing: its repair and
    /// any keyframe request wait in the same queue. The repair deadline stops
    /// while media stops arriving for longer than a frame period, or while no
    /// repair arrives for any frame although this frame's request is overdue,
    /// by up to this much per frame. A frame that is truly lost is dropped
    /// after at most its deadline plus this allowance.
    static let maxStallAllowanceUs: UInt64 = 100_000

    /// Cumulative per-connection loss accounting, safe to read from any
    /// thread (feeds receiver reports and the HUD).
    struct Counters {
        var framesComplete: UInt64 = 0
        var framesDropped: UInt64 = 0
        var fragsReceived: UInt64 = 0
        var fragsLost: UInt64 = 0
        var fragsRepaired: UInt64 = 0
        var nacksSent: UInt64 = 0
        var highestSeq: UInt32 = 0
        var streamEpoch: UInt32 = 0
        var assemblerDepth: UInt8 = 0
        var jitterUs: UInt32 = 0
    }

    let counters = OSAllocatedUnfairLock<Counters>(initialState: Counters())

    private var pending: [UInt32: PendingFrame] = [:]
    private var pendingBytes = 0
    private var latestCompletedSeq: UInt32 = 0
    private var retiredSeq: UInt32 = 0
    private var previousTransit: Double?
    /// When the latest media fragment and the latest repair arrived; see
    /// `maxStallAllowanceUs`.
    private var lastArrivalUs: UInt64?
    private var lastRepairUs: UInt64?
    private var jitter = 0.0
    /// The host stamps a per-pipeline-run `stream_epoch` into each fragment header. When it
    /// changes we know the host restarted (seq reset toward 1) and drop the old stream's state
    /// immediately — more reliable than inferring a restart from a sequence gap. `nil` until the
    /// first fragment; `0` from older hosts that don't set it (those rely on `streamRestartGap`).
    private var currentEpoch: UInt32?

    /// A backward jump in sequence numbers larger than this means the host restarted its
    /// pipeline (capture-display switch, resolution change, etc.) and reset seq toward 0.
    /// Without this, every frame of the new stream is `<= latestCompletedSeq` and gets
    /// dropped forever — the app appears frozen until it's force-quit.
    private static let streamRestartGap: UInt32 = 256

    /// Consecutive stale-epoch drops, and the count after which the epoch we are
    /// holding is treated as bogus and re-synced to whatever is arriving.
    ///
    /// One corrupted or spoofed fragment carrying a high epoch would otherwise
    /// latch an epoch the host will never reach, and every real fragment would
    /// be dropped for the rest of the session. Nothing would notice: control
    /// heartbeats keep flowing, so the liveness watchdog stays happy while the
    /// video is frozen. Genuine stragglers from a previous run can't trip this,
    /// because the new run's fragments interleave and reset the streak.
    private var staleEpochStreak: UInt32 = 0
    private static let epochResyncThreshold: UInt32 = 512

    struct PendingFrame {
        let fragmentCount: UInt16
        let isKeyframe: Bool
        let captureTimestampUs: UInt64
        var fragments: [UInt16: Data]
        var byteCount: Int
        let createdAt: UInt64  // monotonic microseconds
        var contiguous: UInt16 = 0
        var highestIndex: UInt16 = 0
        var repaired: UInt64 = 0
        var gapAt: UInt64?
        var deadline: UInt64?
        var retryAt: UInt64?
        var requestedAt: UInt64?
        var completedAt: UInt64?
        var stallAllowanceUsed: UInt64 = 0
        var stallCountedUntil: UInt64 = 0
        /// Indices already requested; each is requested when first seen
        /// missing and at most once more by the frame's single retry.
        var requested = Set<UInt16>()
        var retried = false
        var keyframeRequested = false

        var isComplete: Bool {
            fragments.count == Int(fragmentCount)
        }
    }

    func addFragment(
        seq: UInt32,
        index: UInt16,
        count: UInt16,
        epoch: UInt32,
        isKeyframe: Bool,
        captureTimestampUs: UInt64,
        payload: Data,
        isRetransmit: Bool = false
    ) {
        // Primary restart signal: the host's stream epoch increases monotonically per pipeline
        // run. A HIGHER epoch means a brand-new run — drop all old state instantly so a fast
        // restart (within the seq-gap window) can't stall the stream. A LOWER epoch is a stale or
        // reordered fragment from the previous run; drop it (never roll currentEpoch backward, or
        // late old-run packets would ping-pong the reset against the new run). Older hosts send a
        // constant 0 here, so this stays a no-op for them and the seq-gap fallback takes over.
        if let current = currentEpoch {
            if epoch > current {
                onDiagnostic?("Stream epoch changed (\(current) -> \(epoch)) — resetting reassembly")
                reset()
                currentEpoch = epoch
            } else if epoch < current {
                staleEpochStreak += 1
                guard staleEpochStreak >= Self.epochResyncThreshold else { return }
                // Nothing has been accepted across a long run of drops, so the
                // epoch we are holding can't be the live one. Re-sync to the
                // stream that is actually arriving rather than stay frozen.
                onDiagnostic?(
                    "Epoch \(current) never resumed after \(staleEpochStreak) dropped fragments"
                        + " — re-syncing to epoch \(epoch)"
                )
                reset()
                currentEpoch = epoch
            } else {
                staleEpochStreak = 0
            }
        } else {
            currentEpoch = epoch
        }

        // Any current-stream datagram shows media flowing; any replacement shows
        // repairs flowing, including one that lost the race to its original.
        let now = nowUs()
        if repairEnabled { resumeAfterStall(at: now) }
        lastArrivalUs = now
        if isRetransmit { lastRepairUs = now }

        let latestRetired = max(latestCompletedSeq, retiredSeq)
        if latestRetired > 0 {
            if seq == latestRetired {
                // Duplicate fragment for the frame we just completed — ignore.
                return
            }
            if seq < latestRetired {
                if latestRetired - seq > Self.streamRestartGap {
                    // Host restarted its stream (seq reset toward 0). Drop the old stream's
                    // state and accept this fragment as the start of the new stream.
                    onDiagnostic?("Stream restart detected (seq \(latestCompletedSeq) -> \(seq)) — resetting reassembly")
                    let epoch = currentEpoch
                    reset()
                    currentEpoch = epoch
                } else {
                    // Genuinely stale/late fragment from the current stream.
                    return
                }
            }
        }

        guard count > 0 else {
            onDiagnostic?("Dropped fragment for seq=\(seq) with zero fragment count")
            return
        }
        guard count <= Self.maxFragmentCount else {
            onDiagnostic?("Dropped fragment for seq=\(seq): fragment count \(count) exceeds cap \(Self.maxFragmentCount)")
            return
        }
        guard index < count else {
            onDiagnostic?("Dropped fragment for seq=\(seq) with out-of-range index \(index)/\(count)")
            return
        }

        tick(at: now)
        if repairEnabled && retiredSeq > 0 && seq <= retiredSeq { return }
        if let existing = pending[seq], existing.fragmentCount != count {
            onDiagnostic?("Ignored fragment for seq=\(seq) with mismatched count \(count) (frame has \(existing.fragmentCount))")
            return
        }
        if pending[seq]?.fragments[index] != nil { return }
        guard payload.count <= Self.maxPendingBytes else { return }
        while (pending[seq] == nil && pending.count >= Self.maxPendingFrames)
            || pendingBytes + payload.count > Self.maxPendingBytes {
            guard let oldest = pending.keys.min() else { break }
            onDiagnostic?("Dropped partial frame seq=\(oldest) to admit seq=\(seq) (capacity)")
            dropPending(oldest)
            if repairEnabled {
                retiredSeq = max(retiredSeq, oldest)
                drainReady(at: now)
                if seq <= retiredSeq { return }
            }
        }
        var frame = pending.removeValue(forKey: seq) ?? PendingFrame(
            fragmentCount: count, isKeyframe: isKeyframe, captureTimestampUs: captureTimestampUs,
            fragments: [:], byteCount: 0, createdAt: now
        )
        frame.fragments[index] = payload
        frame.byteCount += payload.count
        frame.highestIndex = max(frame.highestIndex, index)
        while frame.contiguous < count && frame.fragments[frame.contiguous] != nil {
            frame.contiguous += 1
        }
        pendingBytes += payload.count
        if isRetransmit {
            frame.repaired += 1
        } else {
            // RFC 3550 A.8, using microseconds in both clock domains. A fixed
            // clock offset cancels when successive transit times are diffed.
            let transit = Double(now) - Double(captureTimestampUs)
            if let previousTransit { jitter += (abs(transit - previousTransit) - jitter) / 16 }
            previousTransit = transit
        }
        if frame.isComplete { frame.completedAt = now }
        pending[seq] = frame
        counters.withLock {
            if !isRetransmit { $0.fragsReceived += 1 }
            $0.highestSeq = max($0.highestSeq, seq)
            $0.streamEpoch = epoch
            $0.jitterUs = UInt32(clamping: UInt64(jitter))
        }

        if repairEnabled {
            if !frame.isComplete && frame.contiguous < frame.highestIndex {
                beginGap(seq, through: frame.highestIndex, now: now)
            }
            if frame.isComplete {
                // Completion of a newer frame also proves older trailing
                // fragments are absent, even if no later index exposed them.
                for older in pending.keys.sorted() where older < seq {
                    if let partial = pending[older], !partial.isComplete {
                        beginGap(older, through: partial.fragmentCount - 1, now: now)
                    }
                }
            }
            drainReady(at: now)
        } else if frame.isComplete {
            deliver(seq)
            for stale in pending.keys where stale <= seq { dropPending(stale) }
        }
        updateDepth()
    }

    func reset() {
        pending.removeAll()
        pendingBytes = 0
        latestCompletedSeq = 0
        retiredSeq = 0
        previousTransit = nil
        lastArrivalUs = nil
        lastRepairUs = nil
        jitter = 0
        currentEpoch = nil
        staleEpochStreak = 0
        counters.withLock { $0 = Counters() }
    }

    /// Called by the receiver's timer even when the last fragment was lost.
    func tick(at instant: UInt64? = nil) {
        let now = instant ?? nowUs()
        for seq in pending.keys.sorted() {
            guard let frame = pending[seq], !frame.isComplete else { continue }
            if repairEnabled {
                if let deadline = frame.deadline, now >= deadline + stall(of: frame, at: now) {
                    // Retire in sequence order. Expiring a later partial
                    // first must not make an older repair look stale.
                    if pending.keys.min() == seq {
                        dropPending(seq)
                        retiredSeq = max(retiredSeq, seq)
                        drainReady(at: now)
                    }
                } else if frame.gapAt == nil, now >= frame.createdAt + framePeriodUs {
                    beginGap(seq, through: frame.fragmentCount - 1, now: now)
                } else if let retry = frame.retryAt, now >= retry, !frame.retried {
                    pending[seq]?.retried = true
                    request(seq, through: frame.fragmentCount - 1, again: true, now: now)
                }
            } else if now >= frame.createdAt + 100_000 {
                dropPending(seq)
            }
        }
        if repairEnabled { drainReady(at: now) }
        updateDepth()
    }

    /// How long this waiting frame has been stalled, not yet counted, within
    /// its remaining allowance: media silent beyond a frame period, or its
    /// request overdue while no repair arrives for any frame.
    private func stall(of frame: PendingFrame, at now: UInt64) -> UInt64 {
        guard let gap = frame.gapAt else { return 0 }
        var silentFrom = UInt64.max
        if let last = lastArrivalUs { silentFrom = last + framePeriodUs }
        if let asked = frame.requestedAt {
            let due = max(asked, lastRepairUs ?? 0) + min(rttUs(), 1_000_000) + 5_000
            silentFrom = min(silentFrom, due)
        }
        silentFrom = max(silentFrom, gap, frame.stallCountedUntil)
        guard now > silentFrom else { return 0 }
        return min(now - silentFrom, Self.maxStallAllowanceUs - frame.stallAllowanceUsed)
    }

    /// Traffic arrived: move each waiting frame's deadline past the stall so far.
    private func resumeAfterStall(at now: UInt64) {
        for seq in pending.keys {
            guard let frame = pending[seq], let deadline = frame.deadline else { continue }
            let extra = stall(of: frame, at: now)
            guard extra > 0 else { continue }
            pending[seq]?.deadline = deadline + extra
            pending[seq]?.stallAllowanceUsed += extra
            pending[seq]?.stallCountedUntil = now
        }
    }

    /// The first gap starts the frame's repair deadline and schedules its one
    /// retry. Every gap requests the fragments it newly shows missing: a frame
    /// can lose several fragments far apart, and waiting for the retry to ask
    /// for a later one left too little time before the deadline.
    private func beginGap(_ seq: UInt32, through index: UInt16, now: UInt64) {
        guard let frame = pending[seq] else { return }
        if frame.gapAt == nil {
            let rtt = min(rttUs(), 1_000_000)
            let window = max(8_000, min(25_000, 2 * rtt + framePeriodUs * 3 / 2))
            pending[seq]?.gapAt = now
            pending[seq]?.deadline = now + window
            pending[seq]?.retryAt = now + rtt + 5_000
        }
        request(seq, through: index, again: false, now: now)
    }

    /// A NACK names at most 64 fragments, so a wider gap takes several. A
    /// large frame whose fragments are only late is still repaired or
    /// completed; a keyframe is requested only once a frame is dropped.
    private func request(_ seq: UInt32, through index: UInt16, again: Bool, now: UInt64) {
        guard let frame = pending[seq], frame.contiguous <= index else { return }
        let missing = (frame.contiguous...index).filter {
            frame.fragments[$0] == nil && (again || !frame.requested.contains($0))
        }
        guard !missing.isEmpty else { return }
        pending[seq]?.requested.formUnion(missing)
        if pending[seq]?.requestedAt == nil { pending[seq]?.requestedAt = now }
        for start in stride(from: 0, to: missing.count, by: 64) {
            counters.withLock { $0.nacksSent += 1 }
            onNack?(Nack(streamEpoch: currentEpoch ?? 0, frameSeq: seq, fragCount: frame.fragmentCount,
                         missing: Array(missing[start..<min(start + 64, missing.count)])))
        }
    }

    private func requestKeyframe(_ seq: UInt32) {
        guard pending[seq]?.keyframeRequested == false else { return }
        // A complete queued keyframe restores references after this loss.
        // Keep the repair deadline, but do not request another replacement.
        guard !pending.contains(where: { $0.key > seq && $0.value.isKeyframe && $0.value.isComplete }) else { return }
        pending[seq]?.keyframeRequested = true
        onNeedsKeyframe?(currentEpoch ?? 0, latestCompletedSeq)
    }

    /// Delivers complete frames in order. WiFi can deliver a one-datagram frame
    /// after its successor, and decoding the successor first would leave the
    /// earlier frame stale and the decoder without its reference. A complete
    /// frame that skips a sequence number therefore waits up to half a frame
    /// period for it. The host can also skip a number, so the wait stays
    /// short, and a keyframe depends on no earlier frame.
    private func drainReady(at now: UInt64) {
        while let seq = pending.keys.min(), let frame = pending[seq], frame.isComplete {
            let resolved = max(latestCompletedSeq, retiredSeq)
            if resolved > 0, seq > resolved &+ 1, !frame.isKeyframe,
               now < (frame.completedAt ?? now) + framePeriodUs / 2 {
                break
            }
            deliver(seq)
        }
    }

    private func deliver(_ seq: UInt32) {
        guard let frame = pending.removeValue(forKey: seq) else { return }
        pendingBytes -= frame.byteCount
        var assembled = Data(capacity: frame.byteCount)
        for index in 0..<frame.fragmentCount {
            guard let bytes = frame.fragments[index] else { return }
            assembled.append(bytes)
        }
        latestCompletedSeq = seq
        retiredSeq = max(retiredSeq, seq)
        counters.withLock {
            $0.framesComplete += 1
            $0.fragsRepaired += frame.repaired
        }
        onFrameAssembled?(assembled, seq, frame.captureTimestampUs, frame.isKeyframe)
    }

    private func dropPending(_ seq: UInt32) {
        requestKeyframe(seq)
        if let frame = pending.removeValue(forKey: seq) {
            pendingBytes -= frame.byteCount
            counters.withLock {
                $0.framesDropped += 1
                $0.fragsLost += UInt64(max(Int(frame.fragmentCount) - frame.fragments.count, 0))
            }
        }
    }

    private func updateDepth() {
        let depth = UInt8(clamping: pending.values.filter { !$0.isComplete }.count)
        counters.withLock { $0.assemblerDepth = depth }
    }
}
