// Capture measurements, free of ScreenCaptureKit so tests/run.sh can feed
// them synthetic frames: delivered fps over a fixed window, and the content
// marker the probe draws and reads back from captured and decoded frames.
import Foundation

enum FrameRate {
    /// The measured window starts this long after the first frame.
    static let warmup = 1.0

    /// A fixed window inside a run of `seconds`: from 1 s after the first
    /// frame, 5 s long (shorter only for runs under 7 s).
    static func window(first: Double, seconds: Double) -> (start: Double, length: Double) {
        (first + warmup, max(1, min(5, seconds - warmup - 1)))
    }

    /// Frames per second among `times` (seconds) inside [start, start + length).
    static func inWindow(_ times: [Double], start: Double, length: Double) -> Double {
        guard length > 0 else { return 0 }
        return Double(times.filter { $0 >= start && $0 < start + length }.count) / length
    }

    /// About 0.9× the target: at least 108 for 120.
    static func meets(_ fps: Double, target: Int) -> Bool { fps >= 0.9 * Double(target) }
}

/// Chooses which frames to encode when the encoder runs below the capture
/// rate, from the frames' presentation timestamps (not when their callbacks
/// ran, which jitter). A frame is taken once its timestamp reaches the next
/// slot, less 40% of an interval of slack, so jitter of up to 40% of an
/// interval never drops a frame at the full rate, while 120 fps input still
/// halves cleanly to 60. After a gap the schedule restarts instead of
/// bursting to catch up.
struct FramePacer {
    let interval: Double
    private var next: Double?

    init(fps: Int) { interval = 1 / Double(max(fps, 1)) }

    mutating func take(_ time: Double) -> Bool {
        guard let due = next else {
            next = time + interval
            return true
        }
        guard time >= due - 0.4 * interval else { return false }
        next = time > due + 0.5 * interval ? time + interval : due + interval
        return true
    }
}

/// A row of black and white blocks the probe draws in its window: a sync
/// pattern, the low 16 bits of the display the window is on, and a frame
/// counter. Read back from frames, it proves they show the probe's window on
/// the captured display, and that the content changed between them.
enum Marker {
    static let blockPt = 12.0
    static let sync: [Bool] = [true, false, true, true, false, true, false, false]
    static let count = sync.count + 32

    static func bits(display: UInt32, counter: UInt32) -> [Bool] {
        func bits16(_ value: UInt16) -> [Bool] { (0..<16).map { (value >> UInt16(15 - $0)) & 1 == 1 } }
        return sync + bits16(UInt16(truncatingIfNeeded: display)) + bits16(UInt16(truncatingIfNeeded: counter))
    }

    /// The pixel at the centre of each block, for a marker whose top-left
    /// corner is at (`x`, `y`) pixels with blocks `blockPx` wide.
    static func centres(x: Double, y: Double, blockPx: Double) -> [(Int, Int)] {
        (0..<count).map { (Int(x + (Double($0) + 0.5) * blockPx), Int(y + blockPx / 2)) }
    }

    /// Luma at each block's centre back to (display, counter), or nil unless
    /// the sync blocks read back clearly.
    static func decode(_ luma: [UInt8]) -> (display: UInt16, counter: UInt16)? {
        guard luma.count == count else { return nil }
        let ones = zip(sync, luma).filter { $0.0 }.map { Int($0.1) }
        let zeros = zip(sync, luma).filter { !$0.0 }.map { Int($0.1) }
        guard let low = ones.min(), let high = zeros.max(), low - high >= 64 else { return nil }
        let threshold = (ones.reduce(0, +) / ones.count + zeros.reduce(0, +) / zeros.count) / 2
        let bits = luma.map { Int($0) > threshold }
        func value(_ range: Range<Int>) -> UInt16 { range.reduce(UInt16(0)) { $0 << 1 | (bits[$1] ? 1 : 0) } }
        return (value(sync.count..<sync.count + 16), value(sync.count + 16..<count))
    }
}

/// Markers read from a sequence of frames.
struct MarkerStats {
    let display: UInt16
    var valid = 0
    var unreadable = 0
    var wrongDisplay = 0
    /// Frames whose counter moved on from the previous frame's.
    var changes = 0
    /// Frames whose counter went back (mod 65536).
    var backwards = 0
    private var last: UInt16?

    init(display: UInt32) { self.display = UInt16(truncatingIfNeeded: display) }

    mutating func add(_ marker: (display: UInt16, counter: UInt16)?) {
        guard let marker else { unreadable += 1; return }
        guard marker.display == display else { wrongDisplay += 1; return }
        valid += 1
        if let last {
            let step = marker.counter &- last
            if step != 0 {
                if step < 0x8000 { changes += 1 } else { backwards += 1 }
            }
        }
        last = marker.counter
    }

    /// Every frame shows this display's marker, never going back.
    var proves: Bool { valid > 0 && unreadable == 0 && wrongDisplay == 0 && backwards == 0 }

    var summary: String { "valid=\(valid) unreadable=\(unreadable) wrong_display=\(wrongDisplay) changes=\(changes) backwards=\(backwards)" }
}

/// Decoded frames against the frames submitted to the encoder, by their
/// markers: each decoded frame must be a submitted frame of this display,
/// in submission order, never repeated (so a frozen or reordering decoder
/// fails). Up to `allowedMissing` submitted frames may never come out: 1% of
/// them, and at least 2.
struct DecodedMatch {
    var matched = 0
    var missing = 0
    var unreadable = 0
    var wrongDisplay = 0
    /// Repeated, reordered or never submitted.
    var outOfSequence = 0
    var submitted = 0

    var allowedMissing: Int { max(2, submitted / 100) }
    var passes: Bool { matched > 0 && unreadable == 0 && wrongDisplay == 0 && outOfSequence == 0 && missing <= allowedMissing }
    var summary: String {
        "matched=\(matched)/\(submitted) missing=\(missing) (allowed \(allowedMissing)) unreadable=\(unreadable) wrong_display=\(wrongDisplay) out_of_sequence=\(outOfSequence)"
    }

    init(submitted: [(display: UInt16, counter: UInt16)?], decoded: [(display: UInt16, counter: UInt16)?], display: UInt32) {
        let id = UInt16(truncatingIfNeeded: display)
        self.submitted = submitted.count
        var next = 0 // the first submitted frame not yet matched
        for frame in decoded {
            guard let frame else { unreadable += 1; continue }
            guard frame.display == id else { wrongDisplay += 1; continue }
            guard let index = submitted[next...].firstIndex(where: { $0?.display == id && $0?.counter == frame.counter }) else {
                outOfSequence += 1
                continue
            }
            matched += 1
            next = index + 1
        }
        missing = submitted.count - matched
    }
}
