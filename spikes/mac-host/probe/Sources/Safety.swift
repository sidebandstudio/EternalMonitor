// Safety and deadline logic, kept free of AppKit, displays and event posting
// so tests/run.sh can exercise all of it without touching the session: what
// makes a system-path pointer event safe (the probe's window topmost over
// the whole virtual display, points inset from its edges), the event tap's
// filter, the posted-versus-received matcher, child processes with real
// deadlines, and the probe's hard self-deadline.
import CoreGraphics
import Darwin
import Foundation

// MARK: Event tags

/// `eventSourceUserData` on every event the probe posts: a random run tag in
/// the upper 48 bits and the event's sequence number in the lower 16. Real
/// input carries 0.
enum ProbeTag {
    static func newRun() -> UInt64 { UInt64.random(in: 1...((1 << 48) - 1)) }
    static func make(run: UInt64, seq: Int) -> Int64 { Int64(bitPattern: run << 16 | UInt64(seq & 0xffff)) }
    static func run(_ userData: Int64) -> UInt64 { UInt64(bitPattern: userData) >> 16 }
    static func seq(_ userData: Int64, run: UInt64) -> Int? {
        userData != 0 && self.run(userData) == run ? Int(UInt64(bitPattern: userData) & 0xffff) : nil
    }
}

/// One name per event type, shared by CGEventType and NSEvent.EventType
/// (their raw values agree for every type the probe posts).
func eventName(_ raw: UInt) -> String {
    switch raw {
    case 1: "leftDown"; case 2: "leftUp"; case 3: "rightDown"; case 4: "rightUp"; case 5: "moved"
    case 6: "leftDragged"; case 7: "rightDragged"; case 10: "keyDown"; case 11: "keyUp"; case 12: "flagsChanged"
    case 22: "scroll"; case 23: "tabletPoint"; case 24: "tabletProximity"; case 25: "otherDown"; case 26: "otherUp"
    default: "type\(raw)"
    }
}

// MARK: Where system-path pointer events land

/// One on-screen window, as CGWindowListCopyWindowInfo lists it (front to
/// back), bounds in CG global coordinates.
struct WindowInfo {
    let number: Int
    let pid: Int32
    let layer: Int
    let bounds: CGRect
    let owner: String
}

/// The window server delivers mouse, scroll and pen events to the window
/// under the pointer when they arrive, whatever is focused. So a system-path
/// pointer event is safe only where the probe's own window is topmost.
enum PointerSafety {
    /// Points stay at least this far inside the virtual display.
    static let insetPt = 8.0

    /// Nil when the probe's window (`ownWindow`, at `minLayer` or above)
    /// covers exactly all of `display`, and no other process's window is
    /// above it anywhere on that display; otherwise why not.
    static func coverProblem(_ windows: [WindowInfo], display: CGRect, ownPID: Int32, ownWindow: Int, minLayer: Int) -> String? {
        guard let index = windows.firstIndex(where: { $0.number == ownWindow }) else {
            return "the probe window \(ownWindow) is not on screen"
        }
        let own = windows[index]
        guard own.pid == ownPID else { return "window \(ownWindow) belongs to pid \(own.pid)" }
        guard own.layer >= minLayer else { return "the probe window is at layer \(own.layer), below shielding (\(minLayer))" }
        let edges = [own.bounds.minX - display.minX, own.bounds.minY - display.minY,
                     own.bounds.maxX - display.maxX, own.bounds.maxY - display.maxY]
        guard edges.allSatisfy({ abs($0) <= 0.5 }) else {
            return "the probe window \(own.bounds) does not exactly cover the display \(display)"
        }
        for window in windows[..<index] where window.pid != ownPID && !window.bounds.intersection(display).isEmpty {
            return "window \(window.number) of \(window.owner) (pid \(window.pid), layer \(window.layer)) is above the probe window on the display"
        }
        return nil
    }

    /// The point is inside `display` by at least `insetPt` on every side.
    static func inset(_ point: CGPoint, _ display: CGRect) -> Bool {
        display.insetBy(dx: insetPt, dy: insetPt).contains(point)
    }
}

// MARK: The event tap's filter (never what makes posting safe)

enum TapDecision: Equatable {
    /// Not the probe's: real input or the system's own. Untouched.
    case passThrough
    /// A CGEventPostToPid event, addressed to this process only.
    case passOwnPid
    /// A system-path pointer event that passed the filter.
    case deliver
    /// A system-path event the filter refuses: dropped, and a failure.
    case swallow(String)

    var swallowed: Bool {
        if case .swallow = self { true } else { false }
    }
}

struct TapState {
    /// The tag of the system-path run the tap is guarding.
    var systemRun: UInt64?
    /// The tag of the postToPid run.
    var pidRun: UInt64?
    /// The virtual display's bounds, covered by the probe's window. Without
    /// it no pointer event passes.
    var display: CGRect?
    var frontmost = false
    var ownPID: Int32
}

enum TapPolicy {
    static let forbiddenFlags: CGEventFlags = [.maskCommand, .maskControl, .maskAlternate, .maskSecondaryFn]
    static let keyTypes: Set<CGEventType> = [.keyDown, .keyUp]
    static let pointerTypes: Set<CGEventType> = [
        .mouseMoved, .leftMouseDown, .leftMouseUp, .leftMouseDragged, .rightMouseDown, .rightMouseUp, .rightMouseDragged,
        .otherMouseDown, .otherMouseUp, .otherMouseDragged, .scrollWheel, .tabletPointer, .tabletProximity,
    ]

    /// Real input: someone is using the Mac, so the run stops. Tablet events
    /// don't count: there is no tablet, and macOS may derive them from the
    /// probe's own pen events.
    static func isInput(_ type: CGEventType) -> Bool {
        keyTypes.contains(type) || type == .flagsChanged
            || (pointerTypes.contains(type) && type != .tabletPointer && type != .tabletProximity)
    }

    /// A filter at the last tap before delivery. Safety comes from where the
    /// event is posted (PointerSafety), since macOS can turn a tap off at any
    /// time; this only drops what slipped past that and counts it. A
    /// system-path event passes only if it is a pointer event inside the
    /// virtual display, with the probe frontmost and macOS not annotating it
    /// for another process. Keys are never posted on the system path.
    static func decide(type: CGEventType, userData: Int64, location: CGPoint, flags: CGEventFlags,
                       targetPID: Int32, state: TapState) -> TapDecision {
        guard userData != 0 else { return .passThrough }
        let run = ProbeTag.run(userData)
        if run == state.pidRun { return .passOwnPid }
        guard run == state.systemRun else { return .passThrough }
        if !flags.intersection(forbiddenFlags).isEmpty {
            return .swallow("modifier flags 0x\(String(flags.rawValue, radix: 16)) on the system path")
        }
        if keyTypes.contains(type) { return .swallow("keys are never posted on the system path") }
        guard pointerTypes.contains(type) else { return .swallow("\(eventName(UInt(type.rawValue))) is not allowed on the system path") }
        guard let display = state.display, display.contains(location) else {
            return .swallow(String(format: "(%.2f,%.2f) is outside the virtual display", location.x, location.y))
        }
        guard state.frontmost else { return .swallow("the probe is not frontmost") }
        guard targetPID == 0 || targetPID == state.ownPID else { return .swallow("macOS would deliver it to pid \(targetPID)") }
        return .deliver
    }
}

// MARK: Matching what arrived against what was posted

struct PostedEvent {
    let seq: Int
    let name: String
    let type: String
    /// CG global coordinates, for events whose location matters.
    let point: CGPoint?
    let keyCode: Int?
    let chars: String?
}

struct SeenEvent {
    /// From the event's tag; nil when it carries none of this run's.
    let seq: Int?
    let type: String
    let point: CGPoint?
    let keyCode: Int?
    let chars: String?
}

struct MatchReport {
    var expected = 0
    var matched = 0
    var missing: [Int] = []
    var mismatched: [String] = []
    var outOfOrder: [Int] = []
    var untagged = 0
    var maxErrorPt = 0.0

    var passed: Bool { expected > 0 && matched == expected && mismatched.isEmpty && missing.isEmpty && outOfOrder.isEmpty }

    var summary: String {
        var parts = ["matched \(matched)/\(expected)", String(format: "max error %.2f pt", maxErrorPt)]
        if !missing.isEmpty { parts.append("missing #\(missing.map(String.init).joined(separator: ",#"))") }
        if !mismatched.isEmpty { parts.append("wrong: \(mismatched.joined(separator: " | "))") }
        if !outOfOrder.isEmpty { parts.append("out of order #\(outOfOrder.map(String.init).joined(separator: ",#"))") }
        if untagged > 0 { parts.append("\(untagged) untagged ignored") }
        return parts.joined(separator: ", ")
    }
}

enum SequenceMatcher {
    /// Every posted event must arrive, matched by tag and sequence number:
    /// the same type, within `tolerancePt` of where it was sent, and in order.
    /// Extra copies with the same sequence number are allowed.
    static func match(_ posted: [PostedEvent], _ seen: [SeenEvent], tolerancePt: Double = 1) -> MatchReport {
        var report = MatchReport()
        report.expected = posted.count
        report.untagged = seen.filter { $0.seq == nil }.count
        var last = -1
        for event in posted {
            let candidates = seen.indices.filter { seen[$0].seq == event.seq }
            guard !candidates.isEmpty else {
                report.missing.append(event.seq)
                continue
            }
            var reasons: [String] = []
            var hit: Int?
            for index in candidates {
                if let reason = mismatch(event, seen[index], tolerancePt) { reasons.append(reason) } else { hit = index; break }
            }
            guard let hit else {
                report.mismatched.append("#\(event.seq) \(event.name): \(reasons.joined(separator: "; "))")
                continue
            }
            report.matched += 1
            if let sent = event.point, let landed = seen[hit].point {
                report.maxErrorPt = max(report.maxErrorPt, hypot(landed.x - sent.x, landed.y - sent.y))
            }
            if hit < last { report.outOfOrder.append(event.seq) } else { last = hit }
        }
        return report
    }

    static func mismatch(_ posted: PostedEvent, _ seen: SeenEvent, _ tolerancePt: Double) -> String? {
        if seen.type != posted.type { return "type \(seen.type), expected \(posted.type)" }
        if let sent = posted.point {
            guard let landed = seen.point else { return "no location" }
            let error = hypot(landed.x - sent.x, landed.y - sent.y)
            if error > tolerancePt {
                return String(format: "landed at (%.2f,%.2f), %.2f pt from (%.2f,%.2f)", landed.x, landed.y, error, sent.x, sent.y)
            }
        }
        if let keyCode = posted.keyCode, seen.keyCode != keyCode {
            return "keyCode \(seen.keyCode.map { String($0) } ?? "none"), expected \(keyCode)"
        }
        if let chars = posted.chars, seen.chars != chars { return "chars \"\(seen.chars ?? "")\", expected \"\(chars)\"" }
        return nil
    }
}

// MARK: Deadlines

struct DeadlineExceeded: Error, CustomStringConvertible {
    let seconds: Double
    var description: String { "no answer within \(seconds) s" }
}

final class Once: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false
    func claim() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if done { return false }
        done = true
        return true
    }
}

/// Runs `operation`, giving up after `seconds` even if it never returns (the
/// abandoned operation keeps running; the caller moves on).
func withDeadline<T>(_ seconds: Double, _ operation: @escaping () async throws -> T) async throws -> T {
    try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<T, Error>) in
        let once = Once()
        Task {
            do {
                let value = try await operation()
                if once.claim() { continuation.resume(returning: value) }
            } catch {
                if once.claim() { continuation.resume(throwing: error) }
            }
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + seconds) {
            if once.claim() { continuation.resume(throwing: DeadlineExceeded(seconds: seconds)) }
        }
    }
}

/// A hard deadline on a queue of its own: when it fires, `onFire` runs and
/// the process exits, whatever the main thread is doing. Whatever the process
/// owns, a virtual display included, goes with it.
enum Watchdog {
    private static var timer: DispatchSourceTimer?

    static func arm(seconds: Double, exitCode: Int32 = 124, onFire: @escaping () -> Void) {
        let timer = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "probe.watchdog", qos: .userInteractive))
        timer.schedule(deadline: .now() + seconds)
        timer.setEventHandler {
            // If onFire itself hangs, exit anyway 2 s later.
            DispatchQueue.global().asyncAfter(deadline: .now() + 2) { _exit(exitCode) }
            onFire()
            _exit(exitCode)
        }
        timer.resume()
        self.timer = timer
    }
}

/// A child in a process group of its own, with its stdout on a pipe that is
/// only ever read with a deadline. It is stopped by its PID and group, never
/// by name.
final class ChildProcess: @unchecked Sendable {
    let pid: pid_t
    private let fd: Int32
    private let lock = NSLock()
    private var buffer = Data()
    private var reaped = false
    private(set) var exitStatus: Int32?

    private init(pid: pid_t, fd: Int32) {
        self.pid = pid
        self.fd = fd
    }

    deinit { close(fd) }

    static func spawn(_ path: String, _ args: [String]) throws -> ChildProcess {
        var fds: [Int32] = [0, 0]
        guard pipe(&fds) == 0 else { throw POSIXError(.init(rawValue: errno) ?? .EIO) }
        var actions: posix_spawn_file_actions_t?
        posix_spawn_file_actions_init(&actions)
        defer { posix_spawn_file_actions_destroy(&actions) }
        posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_adddup2(&actions, fds[1], 1)
        posix_spawn_file_actions_addinherit_np(&actions, 2)
        var attributes: posix_spawnattr_t?
        posix_spawnattr_init(&attributes)
        defer { posix_spawnattr_destroy(&attributes) }
        // A group of its own (pgid = pid), and no other descriptors inherited.
        posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_CLOEXEC_DEFAULT))
        posix_spawnattr_setpgroup(&attributes, 0)
        let argv: [UnsafeMutablePointer<CChar>?] = ([path] + args).map { strdup($0) } + [nil]
        defer { argv.forEach { free($0) } }
        var pid: pid_t = 0
        let status = posix_spawn(&pid, path, &actions, &attributes, argv, environ)
        close(fds[1])
        guard status == 0 else {
            close(fds[0])
            throw POSIXError(.init(rawValue: status) ?? .EIO)
        }
        _ = fcntl(fds[0], F_SETFL, fcntl(fds[0], F_GETFL) | O_NONBLOCK)
        _ = fcntl(fds[0], F_SETFD, FD_CLOEXEC)
        return ChildProcess(pid: pid, fd: fds[0])
    }

    /// What has been read but not returned as a line, for error messages.
    var pending: String {
        lock.lock()
        defer { lock.unlock() }
        return String(decoding: buffer, as: UTF8.self)
    }

    /// The next line of output, or nil if none arrives within `timeout`
    /// seconds (or the child closes its output first). Never blocks longer.
    func readLine(timeout: Double) -> String? {
        let deadline = Date().addingTimeInterval(timeout)
        while true {
            lock.lock()
            if let newline = buffer.firstIndex(of: UInt8(ascii: "\n")) {
                let line = String(decoding: buffer[buffer.startIndex..<newline], as: UTF8.self)
                buffer.removeSubrange(buffer.startIndex...newline)
                lock.unlock()
                return line
            }
            lock.unlock()
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { return nil }
            var poller = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
            let ready = poll(&poller, 1, Int32(min(remaining, 0.05) * 1000) + 1)
            guard ready > 0 else { continue }
            var chunk = [UInt8](repeating: 0, count: 256)
            let count = read(fd, &chunk, chunk.count)
            if count > 0 {
                lock.lock()
                buffer.append(contentsOf: chunk[0..<count])
                lock.unlock()
            } else if count == 0 || (errno != EAGAIN && errno != EINTR) {
                return nil // closed without a full line
            }
        }
    }

    /// Reaps the child if it has exited.
    var isRunning: Bool {
        lock.lock()
        defer { lock.unlock() }
        if reaped { return false }
        var status: Int32 = 0
        if waitpid(pid, &status, WNOHANG) == pid {
            reaped = true
            exitStatus = status
            return false
        }
        return true
    }

    /// Seconds until the child exited, or nil if it is still running after `timeout`.
    func waitExit(timeout: Double) -> Double? {
        let start = Date()
        while isRunning {
            if Date().timeIntervalSince(start) >= timeout { return nil }
            usleep(2_000)
        }
        return Date().timeIntervalSince(start)
    }

    /// Sends `signal` to the child's process group and to the child.
    func signal(_ signal: Int32) {
        guard isRunning else { return }
        kill(-pid, signal)
        kill(pid, signal)
    }

    /// SIGKILL if still running, then reap, within `timeout` seconds.
    @discardableResult
    func stop(timeout: Double = 2) -> Bool {
        signal(SIGKILL)
        return waitExit(timeout: timeout) != nil
    }
}

/// The PIDs of the probe's own children, so a deadline, a signal or
/// probe.sh can stop them by PID. `file` (in probe.sh's run directory)
/// lists every child ever started.
enum ChildRegistry {
    private static let lock = NSLock()
    private static var pids: Set<pid_t> = []
    static var file: String?

    static func add(_ pid: pid_t) {
        lock.lock()
        defer { lock.unlock() }
        pids.insert(pid)
        if let file, let handle = FileHandle(forWritingAtPath: file) ?? {
            FileManager.default.createFile(atPath: file, contents: nil)
            return FileHandle(forWritingAtPath: file)
        }() {
            handle.seekToEndOfFile()
            handle.write("\(pid)\n".data(using: .utf8)!)
            handle.closeFile()
        }
    }

    static func remove(_ pid: pid_t) {
        lock.lock()
        defer { lock.unlock() }
        pids.remove(pid)
    }

    static func killAll() {
        lock.lock()
        defer { lock.unlock() }
        for pid in pids {
            kill(-pid, SIGKILL)
            kill(pid, SIGKILL)
        }
    }
}
