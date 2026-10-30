import Foundation
import Network
import UIKit
import VideoToolbox
import os

/// Bounded support logs survive disconnects and launches. Disk work stays off the media queues.
final class SupportDiagnostics: @unchecked Sendable {
    static let shared = SupportDiagnostics()
    static let fileLimit = 1_048_576
    private let queue = DispatchQueue(label: "com.eternal.support-log", qos: .utility)
    private let pending = DispatchSemaphore(value: 512)
    private let lossLock = NSLock()
    private var dropped = 0
    private let directory: URL
    private let limit: Int
    private let upload: DiagnosticsUpload
    private let launchID = UUID().uuidString
    private var file: FileHandle?
    private var bytes = 0
    private var storageError: String?
    private let formatter = ISO8601DateFormatter()
    private var startup: [String] = []
    private var recent: [String] = []

    init(directory: URL? = nil, limit: Int = SupportDiagnostics.fileLimit, upload: DiagnosticsUpload = .shared) {
        self.directory = directory ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Diagnostics", isDirectory: true)
        self.limit = limit
        self.upload = upload
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        queue.sync {
            do {
                try FileManager.default.createDirectory(at: self.directory, withIntermediateDirectories: true)
                try rotate()
                try open()
            } catch { storageError = "\(error)" }
        }
    }

    private func url(_ suffix: String = "") -> URL {
        directory.appendingPathComponent("eternal-ipad.log\(suffix)")
    }

    private func open() throws {
        if !FileManager.default.fileExists(atPath: url().path) {
            guard FileManager.default.createFile(atPath: url().path, contents: nil,
                attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication]) else {
                throw CocoaError(.fileWriteUnknown)
            }
        }
        file = try FileHandle(forWritingTo: url())
        bytes = Int(try file!.seekToEnd())
        var resourceURL = url()
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? resourceURL.setResourceValues(values)
    }

    private func rotate() throws {
        try file?.close()
        file = nil
        let manager = FileManager.default
        if manager.fileExists(atPath: url(".2").path) { try manager.removeItem(at: url(".2")) }
        if manager.fileExists(atPath: url(".1").path) { try manager.moveItem(at: url(".1"), to: url(".2")) }
        if manager.fileExists(atPath: url().path) { try manager.moveItem(at: url(), to: url(".1")) }
    }

    func record(_ level: String = "info", _ category: String, _ message: String) {
        let timestamp = Date()
        upload.record(level: level, category: category, message: message, timestamp: timestamp)
        let uptime = ProcessInfo.processInfo.systemUptime
        guard pending.wait(timeout: .now()) == .success else {
            lossLock.lock(); dropped += 1; lossLock.unlock()
            return
        }
        // Bound individual events before enqueueing as well as total queued events.
        let message = Self.redact(String(message.prefix(8192)))
        let category = String(category.prefix(64))
        queue.async {
            defer { self.pending.signal() }
            let line = "\(self.formatter.string(from: timestamp)) uptime=\(String(format: "%.3f", uptime)) launch=\(self.launchID) \(level) [\(category)] \(message)\n"
            if self.startup.count < 100 { self.startup.append(line) }
            self.recent.append(line)
            if self.recent.count > 200 { self.recent.removeFirst(self.recent.count - 200) }
            do {
                let data = Data(line.utf8)
                if self.bytes > 0 && self.bytes + data.count > self.limit {
                    try self.rotate()
                    try self.open()
                }
                guard let file = self.file else { throw CocoaError(.fileWriteUnknown) }
                try file.write(contentsOf: data)
                self.bytes += data.count
            } catch { self.storageError = "\(error)" }
        }
    }

    /// File names are fixed and bounded. Export never reads settings, Keychain or media.
    func report() async -> String {
        let uploadStatus = await upload.status()
        return await withCheckedContinuation { continuation in
            queue.async {
                self.lossLock.lock(); let dropped = self.dropped; self.lossLock.unlock()
                var report = "EternalMonitor iPad support report, schema 1\nSupport ID: \(self.upload.installationID)\nLaunch: \(self.launchID)\nAutomatic upload: \(uploadStatus)\nOutput queue dropped events: \(dropped)\nStorage error: \(self.storageError ?? "none")\nLogs may contain PC/device names and local network addresses.\n\n--- Startup evidence ---\n\(self.startup.joined())"
                for suffix in [".2", ".1", ""] {
                    if let text = try? String(contentsOf: self.url(suffix), encoding: .utf8) {
                        report += "\n--- Log\(suffix) ---\n\(text)"
                    }
                }
                report += "\n--- Recent memory log ---\n\(self.recent.joined())"
                continuation.resume(returning: Self.redact(report))
            }
        }
    }

    func export() async throws -> URL {
        let text = await report()
        return try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do {
                    let url = FileManager.default.temporaryDirectory.appendingPathComponent("EternalMonitor-iPad-diagnostics.txt")
                    try text.write(to: url, atomically: true, encoding: .utf8)
                    continuation.resume(returning: url)
                } catch { continuation.resume(throwing: error) }
            }
        }
    }

    private static let secretPattern = try! NSRegularExpression(
        pattern: #"\b(pairing_code|pairingCode|auth_token(?:_hex)?|authToken|ingest_key|write_key|token)("?\s*[=:]\s*)("[^"]*"|\[[^\]]*\]|[^\s&,;]+)"#,
        options: .caseInsensitive
    )

    static func redact(_ text: String) -> String {
        secretPattern.stringByReplacingMatches(in: text, range: NSRange(text.startIndex..., in: text),
            withTemplate: "$1$2[REDACTED]")
    }

    /// The PC's name, from its HELLO_ACK, often names its owner ("MARIA-DESKTOP"). Uploads leave it out
    /// as the Windows app's do; this iPad's log and report keep it. The lines that log it end it at
    /// their own last field, so a name holding `session_id=` goes whole.
    private static let pcNameLines = [
        (#"^(Session \d+ established with ).*$"#, "$1%COMPUTERNAME%"),
        (#"^(Connected to ).*( session_id=\d+ host_caps=)"#, "$1%COMPUTERNAME%$2"),
    ].map { (try! NSRegularExpression(pattern: $0.0, options: .dotMatchesLineSeparators), $0.1) }

    /// PC names seen this run, least recently seen first: each HELLO_ACK's and each typed Address that
    /// isn't an IP address. Uploads replace them in free text, except inside a longer word, as a field
    /// name or in a field's value. A line is redacted as it is queued, so letting the oldest name go
    /// never uncovers one. `hardware` is this iPad's model and hardware names, never a PC's.
    private static let pcNames = OSAllocatedUnfairLock(
        initialState: (names: [(name: String, pattern: NSRegularExpression)](), hardware: [String]()))
    static let maxPCNames = 64

    /// Words and values the log uses for other things, as on Windows: replacing them would corrupt it.
    private static let commonWords: Set<String> = [
        "user", "users", "admin", "administrator", "owner", "guest", "default", "public", "home", "pc",
        "desktop", "laptop", "notebook", "computer", "workstation", "server", "display", "monitor", "screen",
        "intel", "nvidia", "amd", "radeon", "geforce", "microsoft", "windows", "apple", "qualcomm", "realtek",
        "true", "false", "yes", "off", "none", "auto", "unknown", "null",
    ]
    /// A field's value (`reason=x`) and a name this redaction wrote.
    private static let keptPattern = try! NSRegularExpression(pattern: #"(?<=[\p{L}\p{N}_]=)\S+|%[A-Z]+%"#)

    /// Call it before logging `name`. Names shorter than 3 characters, common words, numbers and
    /// versions (`5060`, `v1.2.3`) and names inside this iPad's hardware names stay, as on Windows.
    static func notePCName(_ name: String) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let lower = name.lowercased()
        let version = lower.hasPrefix("v") ? lower.dropFirst() : Substring(lower)
        let value = version.contains(where: \.isNumber) && version.allSatisfy { $0.isASCII && ($0.isNumber || $0.isPunctuation) }
        guard name.count >= 3, !value, !commonWords.contains(lower) else { return }
        let pattern = try! NSRegularExpression(
            pattern: #"(?<![\p{L}\p{N}_%-])"# + NSRegularExpression.escapedPattern(for: name) + #"(?![\p{L}\p{N}_=%-])"#,
            options: .caseInsensitive)
        pcNames.withLock { state in
            guard !state.hardware.contains(where: { $0.range(of: name, options: .caseInsensitive) != nil }) else { return }
            state.names.removeAll { $0.name.caseInsensitiveCompare(name) == .orderedSame }
            state.names.append((name, pattern))
            if state.names.count > maxPCNames { state.names.removeFirst(state.names.count - maxPCNames) }
        }
    }

    /// This iPad's model or hardware name (`iPad`, `iPad16,5`): a PC name equal to or inside one stays.
    static func noteHardware(_ text: String) {
        guard !text.isEmpty else { return }
        pcNames.withLock { state in
            guard !state.hardware.contains(text) else { return }
            state.hardware.append(text)
            state.names.removeAll { text.range(of: $0.name, options: .caseInsensitive) != nil }
        }
    }

    /// A typed Address names the PC unless it is an IP address.
    static func noteTypedAddress(_ host: String) {
        guard !host.contains(":"), IPv4Address(host) == nil else { return }
        notePCName(host)
        // The bare name of `maria-desktop.local`.
        if let label = host.split(separator: ".").first, label.count < host.count { notePCName(String(label)) }
    }

    /// A session summary uploads as written: its tokens are the backend's contract (#111).
    static func redactForUpload(_ text: String, category: String) -> String {
        var text = redact(text)
        guard category != "summary" else { return text }
        for (pattern, template) in pcNameLines {
            text = pattern.stringByReplacingMatches(in: text, range: NSRange(text.startIndex..., in: text),
                withTemplate: template)
        }
        for (_, pattern) in pcNames.withLock({ $0.names }).sorted(by: { $0.name.count > $1.name.count }) {
            let whole = NSRange(text.startIndex..., in: text)
            let kept = keptPattern.matches(in: text, range: whole).map(\.range)
            let result = NSMutableString(string: text)
            for match in pattern.matches(in: text, range: whole).reversed()
            where !kept.contains(where: { NSIntersectionRange($0, match.range).length > 0 }) {
                result.replaceCharacters(in: match.range, with: "%COMPUTERNAME%")
            }
            text = result as String
        }
        return text
    }

    @MainActor
    func recordDeviceSnapshot() {
        let process = ProcessInfo.processInfo
        var system = utsname()
        uname(&system)
        let machine = withUnsafeBytes(of: &system.machine) { bytes in
            String(decoding: bytes.prefix(while: { $0 != 0 }), as: UTF8.self)
        }
        let info = Bundle.main.infoDictionary ?? [:]
        let screen = UIScreen.main
        #if targetEnvironment(simulator)
        let simulator = true
        #else
        let simulator = false
        #endif
        Self.noteHardware(UIDevice.current.model)
        Self.noteHardware(machine)
        record("info", "device", "app=\(info["CFBundleShortVersionString"] ?? "unknown") build=\(info["CFBundleVersion"] ?? "unknown") hardware=\(machine) simulator=\(simulator) model=\(UIDevice.current.model) OS=\(process.operatingSystemVersionString) CPUs=\(process.processorCount) RAM_bytes=\(process.physicalMemory)")
        record("info", "display", "native_pixels=\(screen.nativeBounds) points=\(screen.bounds) scale=\(screen.scale) native_scale=\(screen.nativeScale) max_fps=\(screen.maximumFramesPerSecond)")
        record("info", "device", "hardware_decode_h264=\(VTIsHardwareDecodeSupported(kCMVideoCodecType_H264)) hardware_decode_hevc=\(VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC)) thermal=\(process.thermalState.rawValue) low_power=\(process.isLowPowerModeEnabled)")
    }
}
