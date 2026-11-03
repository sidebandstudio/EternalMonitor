import Foundation
import os

/// Machine-readable milestones for the automated end-to-end harness
/// (`scripts/e2e_ios.sh`). The simulator mirrors these to a small file so
/// live system-log collection does not distort timing. Enabled only when launched
/// with `EM_E2E_LOG=1`; completely silent otherwise.
enum E2E {
    static let enabled = ProcessInfo.processInfo.environment["EM_E2E_LOG"] == "1"
    private static let logger = Logger(subsystem: "com.eternal.monitor.e2e", category: "milestone")

    #if targetEnvironment(simulator)
    private static let fileLock = NSLock()
    private static let file: FileHandle? = {
        guard enabled else { return nil }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("eternal-e2e.log")
        if !FileManager.default.fileExists(atPath: url.path) {
            guard FileManager.default.createFile(atPath: url.path, contents: nil) else { return nil }
        }
        let handle = try? FileHandle(forWritingTo: url)
        try? handle?.truncate(atOffset: 0)
        return handle
    }()
    #endif

    static func emit(_ message: String) {
        guard enabled else { return }
        logger.log("\(message, privacy: .public)")
        #if targetEnvironment(simulator)
        fileLock.lock()
        defer { fileLock.unlock() }
        try? file?.write(contentsOf: Data((message + "\n").utf8))
        #endif
    }

    /// `EM_AUTOCONNECT=host:port` makes the app connect immediately on launch,
    /// bypassing the connect screen — the harness's way in.
    static var autoconnectTarget: (host: String, port: UInt16)? {
        guard let spec = ProcessInfo.processInfo.environment["EM_AUTOCONNECT"],
              let colon = spec.lastIndex(of: ":"),
              let port = UInt16(spec[spec.index(after: colon)...])
        else { return nil }
        let host = String(spec[..<colon])
        return host.isEmpty ? nil : (host, port)
    }

    /// Simulator builds: `EM_USB_PORT=port` moves the USB listener off 9877
    /// (0 takes any free port). Every simulator on a Mac shares its
    /// loopback, so an app on another simulator holding 9877 would leave
    /// this one's cable "unavailable".
    static var usbListenPort: UInt16 {
        #if targetEnvironment(simulator)
        if let port = ProcessInfo.processInfo.environment["EM_USB_PORT"].flatMap({ UInt16($0) }) {
            return port
        }
        #endif
        return 9877
    }

    static func firstFrame(width: Int, height: Int, link: String = "udp") {
        guard enabled else { return }
        let monotonicMs = DispatchTime.now().uptimeNanoseconds / 1_000_000
        emit("E2E_FIRST_FRAME w=\(width) h=\(height) link=\(link) monotonic_ms=\(monotonicMs)")
    }

    static func linkSwitch(from: String, to: String) {
        guard enabled else { return }
        let monotonicMs = DispatchTime.now().uptimeNanoseconds / 1_000_000
        emit("E2E_LINK_SWITCH from=\(from) to=\(to) monotonic_ms=\(monotonicMs)")
    }

    static func stats(decoded: Int, width: Int, height: Int, fps: Double, counters: FrameAssembler.Counters, decodeDepth: UInt8 = 0) {
        guard enabled else { return }
        let monotonicMs = DispatchTime.now().uptimeNanoseconds / 1_000_000
        emit(
            "E2E_STATS decoded=\(decoded) w=\(width) h=\(height) fps=\(Int(fps)) dropped=\(counters.framesDropped) repaired=\(counters.fragsRepaired) nacks=\(counters.nacksSent) jitter_us=\(counters.jitterUs) monotonic_ms=\(monotonicMs) assembled=\(counters.framesComplete) decode_depth=\(decodeDepth)"
        )
    }

    static func audio(_ stats: AudioStats, timeline: AudioTimeline) {
        guard enabled else { return }
        // The wire stream and sequence range let the test match each loss to
        // a host drop of the same numbering.
        var range = ""
        if let session = timeline.sessionId, let epoch = timeline.epoch,
           let first = timeline.firstSeq, let next = timeline.nextSeq {
            range = " session_id=\(session) epoch=\(epoch) first_seq=\(first) next_seq=\(next)"
        }
        emit("E2E_AUDIO packets=\(stats.packets) decoded=\(stats.decoded) lost=\(stats.lost) buffer_ms=\(stats.bufferMs) rms=\(stats.rms) tone1k_db=\(stats.tone1kDB)\(range)")
    }
}
