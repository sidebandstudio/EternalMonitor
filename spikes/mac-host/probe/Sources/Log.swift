import Foundation

/// Every line goes to the run's log file (and stderr). RESULT lines are the
/// numbers a run is for; PASS/FAIL lines are its checks.
enum Log {
    static let directory: URL = {
        let url = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/EternalMonitorMacProbe", isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }()
    private static var handle: FileHandle?
    private static let lock = NSLock()
    private static let start = Date()
    static var failures = 0

    static func open(mode: String) -> URL {
        let stamp = ISO8601DateFormatter().string(from: Date()).replacingOccurrences(of: ":", with: "")
        let url = directory.appendingPathComponent("\(stamp)-\(mode).log")
        FileManager.default.createFile(atPath: url.path, contents: nil)
        handle = try? FileHandle(forWritingTo: url)
        let latest = directory.appendingPathComponent("latest-\(mode).log")
        try? FileManager.default.removeItem(at: latest)
        try? FileManager.default.createSymbolicLink(at: latest, withDestinationURL: url)
        return url
    }

    static func line(_ text: String) {
        let elapsed = String(format: "%8.3f", Date().timeIntervalSince(start))
        let out = "\(elapsed) \(text)\n"
        lock.lock()
        defer { lock.unlock() }
        FileHandle.standardError.write(out.data(using: .utf8)!)
        handle?.write(out.data(using: .utf8)!)
    }

    static func result(_ name: String, _ fields: [(String, Any)]) {
        line("RESULT \(name) " + fields.map { "\($0.0)=\($0.1)" }.joined(separator: " "))
    }

    static func check(_ ok: Bool, _ what: String) {
        if !ok { failures += 1 }
        line("\(ok ? "PASS" : "FAIL") \(what)")
    }

    /// An informational check: an INFO line, never counted as a failure.
    static func info(_ ok: Bool, _ what: String) {
        line("INFO \(ok ? "pass" : "fail") \(what)")
    }
}

/// p50/p95/p99/max of a list, formatted to one decimal.
func percentiles(_ values: [Double]) -> String {
    guard !values.isEmpty else { return "n=0" }
    let sorted = values.sorted()
    func p(_ q: Double) -> Double { sorted[Int((Double(sorted.count - 1) * q).rounded())] }
    return String(format: "n=%d p50=%.1f p95=%.1f p99=%.1f max=%.1f", sorted.count, p(0.5), p(0.95), p(0.99), sorted.last!)
}

private let timebase: mach_timebase_info_data_t = {
    var info = mach_timebase_info_data_t()
    mach_timebase_info(&info)
    return info
}()

func machToMs(_ ticks: UInt64) -> Double {
    Double(ticks) * Double(timebase.numer) / Double(timebase.denom) / 1e6
}
