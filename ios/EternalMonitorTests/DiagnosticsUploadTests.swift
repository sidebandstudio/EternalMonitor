import XCTest
@testable import EternalMonitor

final class DiagnosticsUploadTests: XCTestCase {
    func testRejectsUnencryptedAndCredentialBearingEndpoints() {
        XCTAssertTrue(DiagnosticsUpload.Configuration(endpoint: URL(string: "https://logs.example/v1/events")!, ingestKey: "write-only", allowHTTP: false).valid)
        for address in ["http://logs.example/v1/events", "https://secret@logs.example/v1/events", "https://logs.example/v1/events?key=secret"] {
            XCTAssertFalse(DiagnosticsUpload.Configuration(endpoint: URL(string: address)!, ingestKey: "write-only", allowHTTP: false).valid)
        }
    }
    func testOutboxPersistsRedactedEventsAndOptOutDeletesPendingUploads() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let suite = UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { try? FileManager.default.removeItem(at: directory); defaults.removePersistentDomain(forName: suite) }
        let config = DiagnosticsUpload.Configuration(endpoint: URL(string: "http://127.0.0.1:1/v1/events")!, ingestKey: "write-only", allowHTTP: true)
        let upload = DiagnosticsUpload(directory: directory, configuration: config, defaults: defaults)
        defer { upload.shutdown() }
        upload.record(level: "error", category: "decode", message: "VTDecodeFrame status=-12909 pairing_code=123456 authToken=secret session_id=987654", timestamp: Date())
        let status = await upload.status()
        XCTAssertTrue(status.contains("pending_batches=1"), status)
        let file = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).first!
        let data = try Data(contentsOf: file)
        let batch = try JSONDecoder().decode(DiagnosticsUpload.Batch.self, from: data)
        XCTAssertTrue(status.contains("installation_id=\(batch.installation_id)"))
        XCTAssertTrue(status.contains("launch_id=\(batch.launch_id)"))
        XCTAssertFalse(batch.events[0].message.contains("123456"))
        XCTAssertFalse(batch.events[0].message.contains("secret"))
        XCTAssertTrue(batch.events[0].message.contains("status=-12909"))
        XCTAssertTrue(batch.events[0].message.contains("session_id=987654"))
        upload.setEnabled(false)
        _ = await upload.status()
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).isEmpty)
        XCTAssertFalse(upload.enabled)
    }

    func testWrongAcknowledgementKeepsOutboxUntilMatchingRetry() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let suite = UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [DiagnosticsUploadProtocol.self]
        let upload = DiagnosticsUpload(directory: directory, configuration: .init(endpoint: URL(string: "https://upload.invalid/v1/events")!, ingestKey: "write-only", allowHTTP: false), defaults: defaults, sessionConfiguration: configuration)
        defer { upload.shutdown(); try? FileManager.default.removeItem(at: directory); defaults.removePersistentDomain(forName: suite) }
        DiagnosticsUploadProtocol.match = false
        upload.record(level: "error", category: "decode", message: "status=-12909", timestamp: Date())
        _ = await upload.status()
        upload.flushSoon()
        var rejected = false
        for _ in 0..<100 {
            if await upload.status().contains("waiting to retry") { rejected = true; break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertTrue(rejected)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).count, 1)
        DiagnosticsUploadProtocol.match = true
        upload.flushSoon()
        var drained = false
        for _ in 0..<100 {
            if await upload.status().contains("pending_batches=0") { drained = true; break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        let finalStatus = await upload.status()
        XCTAssertTrue(drained, finalStatus)
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil).isEmpty)
    }

    func testStorageRecoverySplitsEscapedMessagesIntoAcceptedSizeBatches() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try Data().write(to: directory) // A file blocks outbox writes until storage recovers.
        let suite = UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        let upload = DiagnosticsUpload(directory: directory, configuration: .init(endpoint: URL(string: "http://127.0.0.1:1/v1/events")!, ingestKey: "write-only", allowHTTP: true), defaults: defaults)
        defer { upload.shutdown(); try? FileManager.default.removeItem(at: directory); defaults.removePersistentDomain(forName: suite) }
        let message = String(repeating: "\u{01}", count: 8192)
        for _ in 0..<40 { upload.record(level: "error", category: "storage", message: message, timestamp: Date()) }
        let failedStatus = await upload.status()
        XCTAssertTrue(failedStatus.contains("dropped=8"), failedStatus)
        try FileManager.default.removeItem(at: directory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        upload.record(level: "info", category: "storage", message: "recovered", timestamp: Date())
        _ = await upload.status()
        let files = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
        var count = 0
        for file in files {
            let data = try Data(contentsOf: file)
            XCTAssertLessThan(data.count, 262144)
            let batch = try JSONDecoder().decode(DiagnosticsUpload.Batch.self, from: data)
            XCTAssertLessThanOrEqual(batch.events.count, 4)
            count += batch.events.count
        }
        XCTAssertEqual(count, 33)
    }

    /// The PC's name is often its owner's. Uploads leave it out; the local
    /// log and "Copy logs" keep it.
    func testUploadsLeaveOutThePCNameAndTheLocalLogKeepsIt() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let suite = UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { try? FileManager.default.removeItem(at: directory); defaults.removePersistentDomain(forName: suite) }
        let config = DiagnosticsUpload.Configuration(endpoint: URL(string: "http://127.0.0.1:1/v1/events")!, ingestKey: "write-only", allowHTTP: true)
        let upload = DiagnosticsUpload(directory: directory, configuration: config, defaults: defaults)
        defer { upload.shutdown() }
        let messages = ["Session 41 established with MARIA-LAPTOP",
                        "Connected to Maria's PC session_id=41 host_caps=31 liveness_ms=3000"]
        for message in messages {
            upload.record(level: "info", category: "ctrl", message: message, timestamp: Date())
        }
        _ = await upload.status()
        let uploaded = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
            .flatMap { try JSONDecoder().decode(DiagnosticsUpload.Batch.self, from: Data(contentsOf: $0)).events.map(\.message) }
        XCTAssertEqual(uploaded.sorted(), ["Connected to [PC-NAME] session_id=41 host_caps=31 liveness_ms=3000",
                                           "Session 41 established with [PC-NAME]"])
        let local = SupportDiagnostics(directory: directory.appendingPathComponent("log", isDirectory: true))
        for message in messages { local.record("info", "ctrl", message) }
        let report = await local.report()
        for message in messages { XCTAssertTrue(report.contains(message), report) }
    }
}


private final class DiagnosticsUploadProtocol: URLProtocol, @unchecked Sendable {
    private static let lock = NSLock()
    private static var matches = false
    static var match: Bool {
        get { lock.lock(); defer { lock.unlock() }; return matches }
        set { lock.lock(); matches = newValue; lock.unlock() }
    }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        var body = request.httpBody ?? Data()
        if let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 4096)
            while body.count < 262144 {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                body.append(contentsOf: buffer.prefix(count))
            }
        }
        let batch = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
        let ack = Self.match ? batch?["batch_id"] as? String ?? "missing" : "wrong-batch"
        let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: try! JSONSerialization.data(withJSONObject: ["accepted_batch": ack]))
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}
