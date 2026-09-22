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

    static let maxRepairFrames = 3

    /// A frame may span at most this many fragments. This MUST match the
    /// protocol cap the host fragments against and the wire parser enforces:
    /// a tighter value here silently discards legitimately large access units
    /// (a 1440p/4K scene-change IDR runs past 1024 fragments), and because the
    /// decoder then never receives a sync sample, every later frame is dropped
    /// too — a permanent freeze with healthy heartbeats. `maxPendingBytes` is
    /// the real memory guard; the u16 field alone would allow ≈90 MB.
    static let maxFragmentCount: UInt16 = MediaHeader.maxFragCount
    /// At most this many partial frames in flight; the oldest is dropped first.
    static let maxPendingFrames = 8
    /// Hard ceiling on buffered fragment bytes across all partial frames.
    static let maxPendingBytes = 8 * 1024 * 1024

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
        var nackCount = 0
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

        let now = nowUs()
        tick(at: now)
        if repairEnabled && retiredSeq > 0 && seq <= retiredSeq { return }
        if let existing = pending[seq], existing.fragmentCount != count {
            onDiagnostic?("Ignored fragment for seq=\(seq) with mismatched count \(count) (frame has \(existing.fragmentCount))")
            return
        }
        if pending[seq]?.fragments[index] != nil { return }
        guard payload.count <= Self.maxPendingBytes else { return }
        let limit = repairEnabled ? Self.maxRepairFrames : Self.maxPendingFrames
        while (pending[seq] == nil && pending.count >= limit)
            || pendingBytes + payload.count > Self.maxPendingBytes {
            guard let oldest = pending.keys.min() else { break }
            onDiagnostic?("Dropped partial frame seq=\(oldest) to admit seq=\(seq) (capacity)")
            dropPending(oldest)
            if repairEnabled {
                retiredSeq = max(retiredSeq, oldest)
                drainReady()
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
            drainReady()
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
                if let deadline = frame.deadline, now >= deadline {
                    // Retire in sequence order. Expiring a later partial
                    // first must not make an older repair look stale.
                    if pending.keys.min() == seq {
                        dropPending(seq)
                        retiredSeq = max(retiredSeq, seq)
                        drainReady()
                    }
                } else if frame.gapAt == nil, now >= frame.createdAt + framePeriodUs {
                    beginGap(seq, through: frame.fragmentCount - 1, now: now)
                } else if let retry = frame.retryAt, now >= retry, frame.nackCount < 2 {
                    requestMissing(seq, through: frame.fragmentCount - 1)
                }
            } else if now >= frame.createdAt + 100_000 {
                dropPending(seq)
            }
        }
        if repairEnabled { drainReady() }
        updateDepth()
    }

    private func beginGap(_ seq: UInt32, through index: UInt16, now: UInt64) {
        guard let frame = pending[seq], frame.gapAt == nil else { return }
        let rtt = min(rttUs(), 1_000_000)
        let window = max(8_000, min(25_000, 2 * rtt + framePeriodUs * 3 / 2))
        pending[seq]?.gapAt = now
        pending[seq]?.deadline = now + window
        pending[seq]?.retryAt = now + rtt + 5_000
        requestMissing(seq, through: index)
    }

    private func requestMissing(_ seq: UInt32, through index: UInt16) {
        guard let frame = pending[seq], frame.nackCount < 2 else { return }
        let missing = (0...index).filter { frame.fragments[$0] == nil }
        guard !missing.isEmpty else { return }
        if missing.count > 64 {
            pending[seq]?.nackCount = 2
            requestKeyframe(seq)
            return
        }
        pending[seq]?.nackCount += 1
        counters.withLock { $0.nacksSent += 1 }
        onNack?(Nack(streamEpoch: currentEpoch ?? 0, frameSeq: seq,
                     fragCount: frame.fragmentCount, missing: missing))
    }

    private func requestKeyframe(_ seq: UInt32) {
        guard pending[seq]?.keyframeRequested == false else { return }
        pending[seq]?.keyframeRequested = true
        onNeedsKeyframe?(currentEpoch ?? 0, latestCompletedSeq)
    }

    private func drainReady() {
        while let seq = pending.keys.min(), pending[seq]?.isComplete == true { deliver(seq) }
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
