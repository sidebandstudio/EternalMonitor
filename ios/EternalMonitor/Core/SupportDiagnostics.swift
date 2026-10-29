import Foundation
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
        record("info", "device", "app=\(info["CFBundleShortVersionString"] ?? "unknown") build=\(info["CFBundleVersion"] ?? "unknown") hardware=\(machine) simulator=\(simulator) model=\(UIDevice.current.model) OS=\(process.operatingSystemVersionString) CPUs=\(process.processorCount) RAM_bytes=\(process.physicalMemory)")
        record("info", "display", "native_pixels=\(screen.nativeBounds) points=\(screen.bounds) scale=\(screen.scale) native_scale=\(screen.nativeScale) max_fps=\(screen.maximumFramesPerSecond)")
        record("info", "device", "hardware_decode_h264=\(VTIsHardwareDecodeSupported(kCMVideoCodecType_H264)) hardware_decode_hevc=\(VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC)) thermal=\(process.thermalState.rawValue) low_power=\(process.isLowPowerModeEnabled)")
    }
}
