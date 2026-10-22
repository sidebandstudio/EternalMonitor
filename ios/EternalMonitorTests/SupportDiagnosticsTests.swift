import XCTest
@testable import EternalMonitor

final class SupportDiagnosticsTests: XCTestCase {
    func testRedactsPairingCredentialsWithoutLosingSessionOrErrorCodes() {
        let text = #"pairing_code=123456 auth_token_hex="00112233445566778899aabbccddeeff" token=[1, 2, 3] pairingCode:654321 status=-12909 session_id=123456 "auth_token_hex": "json-secret""#
        let redacted = SupportDiagnostics.redact(text)
        for secret in ["001122", "654321", "[1, 2, 3]", "pairing_code=123456", "json-secret"] {
            XCTAssertFalse(redacted.contains(secret), redacted)
        }
        XCTAssertTrue(redacted.contains("status=-12909 session_id=123456"))
    }

    func testReportSurvivesRelaunchIncludesLatestErrorAndExportsAFile() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let first = SupportDiagnostics(directory: directory)
        first.record("error", "decode", "VTDecodeFrame status=-12909 token=secret")
        let firstReport = await first.report()
        XCTAssertTrue(firstReport.contains("VTDecodeFrame status=-12909"))
        XCTAssertFalse(firstReport.contains("token=secret"))
        let second = SupportDiagnostics(directory: directory)
        second.record("info", "connection", "new launch")
        let report = await second.report()
        XCTAssertTrue(report.contains("VTDecodeFrame status=-12909"))
        XCTAssertTrue(report.contains("new launch"))
        XCTAssertTrue(report.contains("Storage error: none"))
        let export = try await second.export()
        XCTAssertTrue(try String(contentsOf: export).contains("VTDecodeFrame status=-12909"))
    }

    func testStorageFailureKeepsLatestErrorInMemory() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try Data().write(to: directory)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = SupportDiagnostics(directory: directory)
        for event in 0..<120 { store.record("info", "test", "event=\(event)") }
        store.record("error", "decode", "latest-error status=-12909")
        let report = await store.report()
        XCTAssertFalse(report.contains("Storage error: none"))
        XCTAssertTrue(report.contains("latest-error status=-12909"))
    }

    func testRotationBoundsStorageAndRetainsLatestError() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let store = SupportDiagnostics(directory: directory, limit: 1024)
        for event in 0..<100 { store.record("info", "test", "event=\(event) " + String(repeating: "x", count: 100)) }
        store.record("error", "decode", "last-error status=-123")
        let report = await store.report()
        XCTAssertTrue(report.contains("last-error status=-123"))
        let files = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.fileSizeKey])
        XCTAssertEqual(files.count, 3)
        for file in files { XCTAssertLessThanOrEqual(try file.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0, 1024) }
    }

    /// The support log says why the PC asked for its code: the iPad sent
    /// nothing yet, a wrong code, or a saved pairing the PC no longer has.
    @MainActor func testPairingRefusalsSayWhatTheIPadSent() async throws {
        let host = FakeUDPHost()
        let manager = ConnectionManager()
        defer { manager.cancel() }
        manager.connect(host: "127.0.0.1", port: host.port)
        let refused = await Task.detached { host.acceptSession(status: .unauthorized) }.value
        XCTAssertTrue(refused, "the iPad never sent HELLO2")
        for _ in 0..<500 where manager.pairingPrompt == nil { try await Task.sleep(nanoseconds: 10_000_000) }
        manager.pairingPrompt?.code = "123456"
        manager.submitPairing()
        let again = await Task.detached { host.acceptSession(status: .unauthorized) }.value
        XCTAssertTrue(again, "the code was never sent")
        func pairing() -> [String] { manager.diagnostics.filter { $0.category == "pairing" }.map(\.message) }
        for _ in 0..<500 where pairing().count < 2 { try await Task.sleep(nanoseconds: 10_000_000) }
        XCTAssertEqual(pairing(), ["Pairing refused: pairing code needed", "Pairing refused: wrong pairing code"])
        XCTAssertEqual(manager.diagnostics.last { $0.category == "pairing" }?.level, .warning)
    }

    func testPairingRefusalMessages() {
        let cases: [(HelloStatus, Bool, Bool, DiagnosticLevel, String)] = [
            (.unauthorized, false, false, .info, "Pairing refused: pairing code needed"),
            (.unauthorized, false, true, .warning, "Pairing refused: saved pairing no longer matches the PC"),
            (.unauthorized, true, false, .warning, "Pairing refused: wrong pairing code"),
            (.rateLimited, true, false, .warning, "Pairing refused: too many wrong codes, wait 60 seconds"),
        ]
        for (status, codeSent, savedPairing, level, message) in cases {
            let refusal = SupportDiagnostics.pairingRefusal(status, codeSent: codeSent, savedPairing: savedPairing)
            XCTAssertEqual(refusal.level, level)
            XCTAssertEqual(refusal.message, message)
        }
    }
}
