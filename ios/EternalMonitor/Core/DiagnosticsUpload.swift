import Foundation

/// Automatic upload uses a write-only credential. Never put a dashboard/database key in an app.
final class DiagnosticsUpload: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    static let shared = DiagnosticsUpload()
    struct Configuration {
        let endpoint: URL
        let ingestKey: String
        let allowHTTP: Bool
        var valid: Bool {
            (endpoint.scheme == "https" || (allowHTTP && endpoint.scheme == "http")) && endpoint.host != nil && endpoint.user == nil && endpoint.password == nil && endpoint.query == nil && !ingestKey.isEmpty && !ingestKey.contains("\r") && !ingestKey.contains("\n")
        }
        static func load() -> Configuration? {
            let env = ProcessInfo.processInfo.environment
            let info = Bundle.main.infoDictionary ?? [:]
            guard let address = env["ETERNAL_DIAGNOSTICS_ENDPOINT"] ?? info["DiagnosticsEndpoint"] as? String,
                  let endpoint = URL(string: address),
                  let key = env["ETERNAL_DIAGNOSTICS_INGEST_KEY"] ?? info["DiagnosticsIngestKey"] as? String else { return nil }
            let configuration = Configuration(endpoint: endpoint, ingestKey: key, allowHTTP: env["ETERNAL_DIAGNOSTICS_ALLOW_HTTP"] == "1")
            return configuration.valid ? configuration : nil
        }
    }
    struct Event: Codable {
        let sequence: Int
        let at_ms: Int64
        let level: String
        let category: String
        let message: String
    }
    struct Batch: Codable {
        var schema = 1
        let batch_id: String
        let installation_id: String
        let launch_id: String
        let platform: String
        let app_version: String
        let build: String
        let events: [Event]
    }
    private let queue = DispatchQueue(label: "com.eternal.diagnostics-upload", qos: .utility)
    private let capacity = DispatchSemaphore(value: 256)
    private let directory: URL
    private let configuration: Configuration?
    private let defaults: UserDefaults
    private var session: URLSession!
    private var timer: DispatchSourceTimer?
    private var inFlight = false
    private var sequence = 0
    private var pending: [Event] = []
    private var nextAttempt = Date.distantPast
    private var backoff = 5.0
    private var statusText = "Automatic upload is not configured"
    /// Shown to the user as the Support ID: support finds this iPad's uploaded rows by it.
    let installationID: String
    private let launchID = UUID().uuidString
    private var dropped = 0
    private let lossLock = NSLock()
    static let enabledKey = "automaticallyShareDiagnostics"

    init(directory: URL? = nil, configuration: Configuration? = Configuration.load(), defaults: UserDefaults = .standard, sessionConfiguration: URLSessionConfiguration? = nil) {
        self.directory = directory ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("DiagnosticsOutbox", isDirectory: true)
        self.configuration = configuration
        self.defaults = defaults
        let identityKey = "diagnosticsInstallationID"
        let installationID = defaults.string(forKey: identityKey) ?? UUID().uuidString
        defaults.set(installationID, forKey: identityKey)
        self.installationID = installationID
        super.init()
        let config = sessionConfiguration ?? URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 5
        config.timeoutIntervalForResource = 10
        session = URLSession(configuration: config, delegate: self, delegateQueue: nil)
        // A test instance without an endpoint can exercise disk persistence without network traffic.
        if configuration != nil { statusText = "Automatic upload enabled; waiting for events" }
        queue.async {
            do {
                try FileManager.default.createDirectory(at: self.directory, withIntermediateDirectories: true,
                    attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
                var url = self.directory
                var values = URLResourceValues(); values.isExcludedFromBackup = true
                try? url.setResourceValues(values)
            } catch { self.statusText = "Automatic upload outbox unavailable: \(error.localizedDescription)" }
            let timer = DispatchSource.makeTimerSource(queue: self.queue)
            timer.schedule(deadline: .now() + 1, repeating: 5)
            timer.setEventHandler { [weak self] in self?.flush() }
            self.timer = timer
            timer.resume()
        }
    }
    func shutdown() { queue.sync { timer?.cancel(); timer = nil; session.invalidateAndCancel() } }
    var enabled: Bool { defaults.object(forKey: Self.enabledKey) as? Bool ?? true }
    var configured: Bool { configuration != nil }
    func setEnabled(_ enabled: Bool) {
        defaults.set(enabled, forKey: Self.enabledKey)
        queue.async {
            if !enabled {
                self.pending.removeAll()
                self.session.getAllTasks { $0.forEach { $0.cancel() } }
                for url in self.files() { try? FileManager.default.removeItem(at: url) }
                self.statusText = "Automatic upload disabled"
            } else { self.nextAttempt = .distantPast; self.flush() }
        }
    }
    func record(level: String, category: String, message: String, timestamp: Date) {
        guard configured, enabled else { return }
        guard capacity.wait(timeout: .now()) == .success else { lossLock.lock(); dropped += 1; lossLock.unlock(); return }
        let message = SupportDiagnostics.redactForUpload(String(decoding: message.utf8.prefix(8192), as: UTF8.self), category: category)
        queue.async {
            defer { self.capacity.signal() }
            guard self.enabled else { return }
            self.sequence += 1
            self.pending.append(Event(sequence: self.sequence, at_ms: Int64(timestamp.timeIntervalSince1970 * 1000), level: level == "warn" ? "warning" : level, category: String(category.prefix(64)), message: message))
            // Persist promptly so a disconnect or process termination does not erase pending events.
            self.persistPending()
        }
    }
    private func files() -> [URL] {
        ((try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.fileSizeKey])) ?? [])
            .filter { $0.pathExtension == "json" }.sorted { $0.lastPathComponent < $1.lastPathComponent }
    }
    private func persistPending() {
        while !pending.isEmpty {
            let info = Bundle.main.infoDictionary ?? [:]
            // Four maximally escaped 8 KiB messages fit within the API's 256 KiB limit.
            let batch = Batch(batch_id: UUID().uuidString, installation_id: installationID, launch_id: launchID, platform: "ipad", app_version: info["CFBundleShortVersionString"] as? String ?? "unknown", build: info["CFBundleVersion"] as? String ?? "unknown", events: Array(pending.prefix(4)))
            do {
                let data = try JSONEncoder().encode(batch)
                let url = directory.appendingPathComponent("\(batch.events[0].at_ms)-\(batch.batch_id).json")
                try data.write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
                pending.removeFirst(batch.events.count)
                let files = self.files()
                var total = files.reduce(0) { $0 + ((try? $1.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0) }
                var count = files.count
                for url in files where count > 512 || total > 8 * 1024 * 1024 {
                    if inFlight && url == self.activeFile { continue }
                    let size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
                    try FileManager.default.removeItem(at: url); total -= size; count -= 1
                    lossLock.lock(); dropped += 1; lossLock.unlock()
                }
            } catch {
                statusText = "Automatic upload outbox error: \(error.localizedDescription)"
                if pending.count > 32 { let lost = pending.count - 32; pending.removeFirst(lost); lossLock.lock(); dropped += lost; lossLock.unlock() }
                return
            }
        }
    }
    private var activeFile: URL?
    func flushSoon() { queue.async { self.nextAttempt = .distantPast; self.flush() } }
    private func flush() {
        guard enabled, !inFlight, let configuration, Date() >= nextAttempt else { return }
        persistPending()
        guard let url = files().first else { statusText = "Automatic upload connected; outbox empty"; return }
        guard let data = try? Data(contentsOf: url), let batch = try? JSONDecoder().decode(Batch.self, from: data) else {
            // Remove a damaged entry so it cannot permanently block later diagnostics.
            try? FileManager.default.removeItem(at: url)
            lossLock.lock(); dropped += 1; lossLock.unlock(); return
        }
        var request = URLRequest(url: configuration.endpoint)
        request.httpMethod = "POST"
        request.setValue("Bearer \(configuration.ingestKey)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = data
        inFlight = true; activeFile = url
        session.dataTask(with: request) { [weak self] data, response, error in
            guard let self else { return }
            self.queue.async {
                self.inFlight = false; self.activeFile = nil
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                let ack = data.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
                if error == nil, status == 200, ack?["accepted_batch"] as? String == batch.batch_id {
                    try? FileManager.default.removeItem(at: url)
                    self.backoff = 5; self.nextAttempt = .distantPast
                    self.statusText = "Automatic upload connected; last sent \(ISO8601DateFormatter().string(from: Date()))"
                    self.flush()
                } else {
                    self.statusText = "Automatic upload waiting to retry; HTTP=\(status) network=\((error as NSError?)?.code ?? 0) pending_batches=\(self.files().count)"
                    self.backoff = min(300, self.backoff * 2); self.nextAttempt = Date().addingTimeInterval(self.backoff)
                }
            }
        }.resume()
    }
    func status() async -> String {
        await withCheckedContinuation { continuation in queue.async {
            self.lossLock.lock(); let dropped = self.dropped; self.lossLock.unlock()
            continuation.resume(returning: "\(self.statusText); installation_id=\(self.installationID); launch_id=\(self.launchID); dropped=\(dropped); pending_batches=\(self.files().count)")
        } }
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}
